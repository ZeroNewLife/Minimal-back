use anyhow::{Context, Result};
use ethers::{
    prelude::*,
    providers::{Http, Provider},
    types::{Address, U256},
};
use serde::Deserialize;
use std::sync::Arc;

// LI.FI API structures
#[derive(Debug, Deserialize)]
struct LifiQuoteResponse {
    estimate: EstimateData,
    #[serde(rename = "transactionRequest")]
    transaction_request: LifiTransactionRequest, // ПЕРЕИМЕНОВАЛИ
}

#[derive(Debug, Deserialize)]
struct EstimateData {
    #[serde(rename = "toAmount")]
    to_amount: String,
    #[serde(rename = "toAmountMin")]
    to_amount_min: String,
}

#[derive(Debug, Deserialize)]
struct LifiTransactionRequest { // ПЕРЕИМЕНОВАЛИ
    to: String,
    data: String,
    #[serde(default)]
    value: String,
}

// Intent request
#[derive(Debug, Deserialize, Clone)]
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
    let executor_address = std::env::var("EXECUTOR").context("EXECUTOR not set")?;

    let intent = IntentRequest {
        user: "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266".to_string(),
        token_in: "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48".to_string(), // USDC
        token_out: "0x0000000000000000000000000000000000000000".to_string(), // ETH
        amount_in: "1000000".to_string(), // 1 USDC
        slippage_bps: 50,
    };

    execute_intent(&rpc_url, &private_key, &executor_address, intent).await?;

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

    println!("🔍 Resolving recipient address...");
    let recipient_address = resolve_address(&provider, &intent.user).await?;
    println!("✅ Recipient: {:?}", recipient_address);

    let is_eth_in = intent.token_in == "0x0000000000000000000000000000000000000000";

    println!("\n💱 Getting best route from LI.FI...");
    let lifi_response = get_lifi_route(
        &intent.token_in,
        &intent.token_out,
        &intent.amount_in,
        &recipient_address,
    )
    .await?;

    println!("✅ Route found!");
    println!("   Expected output: {} wei", lifi_response.estimate.to_amount);

    let estimated_output = U256::from_dec_str(&lifi_response.estimate.to_amount)?;
    let slippage_factor = U256::from(10000 - intent.slippage_bps);
    let min_amount_out = estimated_output * slippage_factor / U256::from(10000);

    println!("   Our min output (with slippage): {}", min_amount_out);

    let executor_addr: Address = executor_address.parse()?;
    let token_in: Address = intent.token_in.parse()?;
    let token_out: Address = intent.token_out.parse()?;
    let amount_in = U256::from_dec_str(&intent.amount_in)?;

    // ✅ ДОБАВЛЕНО: Approve токенов если это не ETH
    if !is_eth_in {
        println!("\n🔐 Approving tokens...");
        approve_token(&client, token_in, executor_addr, amount_in).await?;
        println!("✅ Tokens approved");
    }

    println!("\n📤 Preparing transaction...");
    println!("   Token In: {:?}", token_in);
    println!("   Token Out: {:?}", token_out);
    println!("   Amount In: {}", amount_in);
    println!("   Min Amount Out: {}", min_amount_out);
    println!("   Recipient: {:?}", recipient_address);

    // Энкодим calldata
    use ethers::abi::{encode, Token};
    
    let intent_tuple = Token::Tuple(vec![
        Token::Address(token_in),
        Token::Address(token_out),
        Token::Uint(amount_in),
        Token::Uint(min_amount_out),
        Token::Address(recipient_address),
    ]);

    let selector = ethers::utils::id("executeIntent((address,address,uint256,uint256,address))");
    let selector_bytes = &selector[..4];

    let encoded_params = encode(&[intent_tuple]);
    let calldata = [selector_bytes, &encoded_params].concat();

    println!("   Calldata: 0x{}", hex::encode(&calldata));

    // Создаем транзакцию
    let mut tx = ethers::types::TransactionRequest::new()
        .to(executor_addr)
        .data(calldata)
        .from(client.address())
        .gas(500000); // ✅ ДОБАВЛЕНО: gas limit

    if is_eth_in {
        println!("   📍 ETH input, sending {} wei", amount_in);
        tx = tx.value(amount_in);
    }

    println!("\n⏳ Sending transaction...");
    let pending = client.send_transaction(tx, None).await?;
    let tx_hash = pending.tx_hash();

    println!("\n✅ Transaction sent!");
    println!("   Tx Hash: {:?}", tx_hash);
    
    let explorer = match chain_id.as_u64() {
        1 => format!("https://etherscan.io/tx/{:?}", tx_hash),
        11155111 => format!("https://sepolia.etherscan.io/tx/{:?}", tx_hash),
        _ => format!("{:?}", tx_hash),
    };
    println!("   {}", explorer);

    println!("\n⏳ Waiting for confirmation...");
    let receipt = pending.await?.context("Transaction failed")?;

    println!("\n🎉 Success!");
    println!("   Block: {:?}", receipt.block_number);
    println!("   Gas: {:?}", receipt.gas_used);
    println!("   Status: {:?}", receipt.status);

    Ok(())
}

// ✅ НОВАЯ ФУНКЦИЯ: Approve ERC20 токенов
async fn approve_token(
    client: &Arc<SignerMiddleware<Provider<Http>, LocalWallet>>,
    token: Address,
    spender: Address,
    amount: U256,
) -> Result<()> {
    use ethers::abi::{encode, Token};

    // approve(address spender, uint256 amount)
    let selector = ethers::utils::id("approve(address,uint256)");
    let selector_bytes = &selector[..4];

    let params = encode(&[Token::Address(spender), Token::Uint(amount)]);
    let calldata = [selector_bytes, &params].concat();

    let tx = ethers::types::TransactionRequest::new()
        .to(token)
        .data(calldata)
        .from(client.address())
        .gas(100000);

    let pending = client.send_transaction(tx, None).await?;
    println!("   Approve tx: {:?}", pending.tx_hash());
    
    let _receipt = pending.await?;
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
        println!("   ENS {} → {:?}", input, addr);
        return Ok(addr);
    }

    anyhow::bail!("Could not resolve address: {}", input)
}

async fn get_lifi_route(
    token_in: &str,
    token_out: &str,
    amount_in: &str,
    user_address: &Address,
) -> Result<LifiQuoteResponse> {
    let client = reqwest::Client::new();

    let from_token = if token_in == "0x0000000000000000000000000000000000000000" {
        "0x0000000000000000000000000000000000000000"
    } else {
        token_in
    };

    let to_token = if token_out == "0x0000000000000000000000000000000000000000" {
        "0x0000000000000000000000000000000000000000"
    } else {
        token_out
    };

    let url = format!(
        "https://li.quest/v1/quote?fromChain=1&toChain=1&fromToken={}&toToken={}&fromAmount={}&fromAddress={:?}",
        from_token, to_token, amount_in, user_address
    );

    println!("   Calling LI.FI API...");

    let response = client.get(&url).send().await?;
    let status = response.status();

    if !status.is_success() {
        let body = response.text().await?;
        anyhow::bail!("LI.FI API error {}: {}", status, body);
    }

    let lifi_response: LifiQuoteResponse = response
        .json()
        .await
        .context("Failed to parse LI.FI response")?;

    Ok(lifi_response)
}