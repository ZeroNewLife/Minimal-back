use anyhow::{Context, Result};
use ethers::{
    abi::Abi,
    prelude::*,
    providers::{Http, Provider},
    types::{Address, Bytes, U256},
};
use serde::{Deserialize, Serialize};
use std::{fs, sync::Arc};

// LI.FI API structures
#[derive(Debug, Deserialize)]
struct LifiQuoteResponse {
    estimate: EstimateData,
    #[serde(rename = "transactionRequest")]
    transaction_request: TransactionRequest,
}

#[derive(Debug, Deserialize)]
struct EstimateData {
    #[serde(rename = "toAmount")]
    to_amount: String,
}

#[derive(Debug, Deserialize)]
struct TransactionRequest {
    to: String,
    data: String,
    #[serde(default)]
    value: String,
}

// Intent request from frontend
#[derive(Debug, Deserialize)]
struct IntentRequest {
    user: String,
    token_in: String,
    token_out: String,
    amount_in: String,
    slippage_bps: u32,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenv::dotenv().ok();

    let rpc_url = std::env::var("RPC_URL").context("RPC_URL not set")?;
    let private_key = std::env::var("PRIVATE_KEY").context("PRIVATE_KEY not set")?;
    let executor_address = std::env::var("EXECUTOR")
        .context("EXECUTOR not set")?;

    let intent = IntentRequest {
        user: "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266".to_string(),
        token_in: "0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238".to_string(), // USDC Sepolia
        token_out: "0x0000000000000000000000000000000000000000".to_string(), // ETH
        amount_in: "1000000".to_string(), // 1 USDC (6 decimals)
        slippage_bps: 50,
    };


    execute_intent(
        &rpc_url,
        &private_key,
        &executor_address,
        intent,
    )
    .await?;

    Ok(())
}

async fn execute_intent(
    rpc_url: &str,
    private_key: &str,
    executor_address: &str,
    intent: IntentRequest,
) -> Result<()> {
    let provider = Provider::<Http>::try_from(rpc_url)?;
    let wallet: LocalWallet = private_key.parse()?;
    let chain_id = provider.get_chainid().await?;
    let wallet = wallet.with_chain_id(chain_id.as_u64());
    let client = Arc::new(SignerMiddleware::new(provider.clone(), wallet));

    let user_address = resolve_address(&provider, &intent.user).await?;
    println!("User address: {:?}", user_address);

    println!("Fetching route from LI.FI...");
    let lifi_response = get_lifi_route(
        &intent.token_in,
        &intent.token_out,
        &intent.amount_in,
        &user_address,
    )
    .await?;

    println!("LI.FI route obtained");
    println!("Estimated output: {}", lifi_response.estimate.to_amount);

    let target: Address = lifi_response.transaction_request.to.parse()?;
    let data = Bytes::from(hex::decode(
        lifi_response.transaction_request.data.trim_start_matches("0x")
    )?);
    
    // Обрабатываем value (может быть пустым или "0x0")
    let value_str = if lifi_response.transaction_request.value.is_empty() 
        || lifi_response.transaction_request.value == "0x0" {
        "0"
    } else {
        &lifi_response.transaction_request.value.trim_start_matches("0x")
    };
    
    let value = if value_str.is_empty() || value_str == "0" {
        U256::zero()
    } else {
        U256::from_str_radix(value_str, 16)?
    };

    // Calculate minAmountOut with slippage
    let estimated_output = U256::from_dec_str(&lifi_response.estimate.to_amount)?;
    let slippage_factor = U256::from(10000 - intent.slippage_bps);
    let min_amount_out = estimated_output * slippage_factor / U256::from(10000);

    println!("Min output: {}", min_amount_out);

    // Load contract ABI
    let abi_json = fs::read_to_string("abi.json").context("Failed to read abi.json")?;
    let abi: Abi = serde_json::from_str(&abi_json)?;

    let executor_addr: Address = executor_address.parse()?;
    let contract = Contract::new(executor_addr, abi, client.clone());

    let token_in: Address = intent.token_in.parse()?;
    let token_out: Address = intent.token_out.parse()?;
    let amount_in = U256::from_dec_str(&intent.amount_in)?;

    println!("Sending transaction to IntentExecutor...");
    
    let tx_hash = contract
        .method::<_, ()>(
            "execute",
            (
                user_address,
                token_in,
                token_out,
                amount_in,
                min_amount_out,
                target,
                data,
            ),
        )?
        .value(value)
        .send()
        .await?
        .tx_hash();

    println!("Transaction sent: {:?}", tx_hash);
    println!("Check on Etherscan: https://etherscan.io/tx/{:?}", tx_hash);

    Ok(())
}

async fn resolve_address(provider: &Provider<Http>, input: &str) -> Result<Address> {
    if let Ok(addr) = input.parse::<Address>() {
        return Ok(addr);
    }

    if input.ends_with(".eth") {
        let addr = provider
            .resolve_name(input)
            .await
            .context(format!("Failed to resolve ENS name: {}", input))?;
        return Ok(addr);
    }

    anyhow::bail!("Could not resolve address: {}", input)
}

async fn get_lifi_route(
    _token_in: &str,
    _token_out: &str,
    _amount_in: &str,
    user_address: &Address,
) -> Result<LifiQuoteResponse> {
    let client = reqwest::Client::new();

    let url = format!(
        "https://li.quest/v1/quote?fromChain=1&toChain=1&fromToken=0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48&toToken=0x0000000000000000000000000000000000000000&fromAmount=1000000&fromAddress={:?}",
        user_address
    );

    println!("Testing LI.FI with Ethereum mainnet...");

    let response = client.get(&url).send().await?;
    let status = response.status();
    let body = response.text().await?;
    
    println!("Status: {}", status);
    
    // Выводим ВЕСЬ ответ
    println!("Full Response:\n{}", body);

    if !status.is_success() {
        anyhow::bail!("LI.FI API error: {}", body);
    }

    let lifi_response: LifiQuoteResponse = serde_json::from_str(&body)?;
    Ok(lifi_response)
}