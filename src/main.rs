use anyhow::{Context, Result};
use ethers::{
    prelude::*,
    providers::{Http, Provider},
    types::{Address, U256},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Deserialize, Clone)]
struct IntentRequest {
    user: String,
    token_in: String,
    token_out: String,
    amount_in: String,
    slippage_bps: u32,
}

#[derive(Debug, Serialize)]
struct QuoteResponse {
    amount_out: String,
    min_amount_out: String,
    price: String,
    dex: String,
    route: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenv::dotenv().ok();

    let rpc_url = std::env::var("RPC_URL").context("RPC_URL not set")?;
    let private_key = std::env::var("PRIVATE_KEY").context("PRIVATE_KEY not set")?;
    let executor_address = std::env::var("EXECUTOR").context("EXECUTOR not set")?;

    // Пример для Sepolia
    let intent = IntentRequest {
        user: "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266".to_string(),
        token_in: "0x0000000000000000000000000000000000000000".to_string(), // ETH
        token_out: "0xDD13E55209Fd76AfE204dBda4007C227904f0a81".to_string(), // WETH
        amount_in: "10000000000000000".to_string(), // 0.1 ETH
        slippage_bps: 300, // 3%
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

    println!("\n🚀 ZapProtocol Intent Execution");
    println!("═══════════════════════════════════════");
    
    let recipient_address = resolve_address(&provider, &intent.user).await?;
    println!("👤 Recipient: {:?}", recipient_address);

    let is_eth_in = intent.token_in == "0x0000000000000000000000000000000000000000";
    let amount_in = U256::from_dec_str(&intent.amount_in)?;

    // Получаем quote от Uniswap
    println!("\n💱 Fetching best route...");
    let quote = get_quote(&client, &intent).await?;
    
    println!("\n📊 QUOTE DETAILS");
    println!("───────────────────────────────────────");
    println!("  DEX: {}", quote.dex);
    println!("  Route: {}", quote.route);
    println!("  Price: {}", quote.price);
    println!("  Expected Output: {} wei", quote.amount_out);
    println!("  Min Output ({}% slippage): {} wei", 
        intent.slippage_bps as f64 / 100.0, 
        quote.min_amount_out
    );

    let executor_addr: Address = executor_address.parse()?;
    let token_in: Address = intent.token_in.parse()?;
    let token_out: Address = intent.token_out.parse()?;
    let min_amount_out = U256::from_dec_str(&quote.min_amount_out)?;

    // Approve если не ETH
    if !is_eth_in {
        println!("\n🔐 Approving tokens...");
        approve_token(&client, token_in, executor_addr, amount_in).await?;
        println!("✅ Approved");
    }

    println!("\n📤 Executing Intent on-chain...");
    
    // Энкодим Intent
    use ethers::abi::{encode, Token};
    
    let intent_tuple = Token::Tuple(vec![
        Token::Address(token_in),
        Token::Address(token_out),
        Token::Uint(amount_in),
        Token::Uint(min_amount_out),
        Token::Address(recipient_address),
    ]);

    let selector = ethers::utils::id("executeIntent((address,address,uint256,uint256,address))");
    let calldata = [&selector[..4], &encode(&[intent_tuple])].concat();

    let mut tx = ethers::types::TransactionRequest::new()
        .to(executor_addr)
        .data(calldata)
        .from(client.address())
        .gas(500000);

    if is_eth_in {
        tx = tx.value(amount_in);
    }

    let pending = client.send_transaction(tx, None).await?;
    let tx_hash = pending.tx_hash();

    println!("\n✅ Transaction Sent!");
    println!("───────────────────────────────────────");
    println!("  Tx Hash: {:?}", tx_hash);
    
    let explorer = match chain_id.as_u64() {
        1 => format!("https://etherscan.io/tx/{:?}", tx_hash),
        11155111 => format!("https://sepolia.etherscan.io/tx/{:?}", tx_hash),
        31337 => format!("Local Anvil tx: {:?}", tx_hash),
        _ => format!("{:?}", tx_hash),
    };
    println!("  Explorer: {}", explorer);

    println!("\n⏳ Waiting for confirmation...");
    let receipt = pending.await?.context("Transaction failed")?;

    println!("\n🎉 SUCCESS!");
    println!("═══════════════════════════════════════");
    println!("  Block: {:?}", receipt.block_number);
    println!("  Gas Used: {:?}", receipt.gas_used);
    println!("  Status: {:?}", receipt.status);

    Ok(())
}

async fn get_quote(
    client: &Arc<SignerMiddleware<Provider<Http>, LocalWallet>>,
    intent: &IntentRequest,
) -> Result<QuoteResponse> {
    use ethers::abi::{encode, Token};

    // Для Sepolia используем mock quote (нет Uniswap V3 Quoter)
    // Для mainnet/fork используем настоящий quoter
    let chain_id = client.get_chainid().await?.as_u64();
    
   if chain_id == 11155111 {
    // Sepolia: разрешаем ТОЛЬКО ETH <-> WETH
    let eth = "0x0000000000000000000000000000000000000000";
    let weth = "0xDD13E55209Fd76AfE204dBda4007C227904f0a81"; // WETH Sepolia

    let valid = 
        (intent.token_in == eth && intent.token_out == weth) ||
        (intent.token_in == weth && intent.token_out == eth);

    if !valid {
        anyhow::bail!(
            "Sepolia demo supports only ETH <-> WETH. Use Anvil fork for real swaps."
        );
    }

    let amount_in = U256::from_dec_str(&intent.amount_in)?;
    let slippage_factor = U256::from(10000 - intent.slippage_bps);
    let min_amount_out = amount_in * slippage_factor / U256::from(10000);

    return Ok(QuoteResponse {
        amount_out: amount_in.to_string(),
        min_amount_out: min_amount_out.to_string(),
        price: "1 ETH = 1 WETH".to_string(),
        dex: "Native WETH".to_string(),
        route: "ETH ↔ WETH (Sepolia demo)".to_string(),
    });
}


    // Mainnet/Anvil - реальный quoter
    let quoter: Address = "0xb27308f9F90D607463bb33eA1BeBb41C27CE5AB6".parse()?;
    
    let token_in_addr: Address = if intent.token_in == "0x0000000000000000000000000000000000000000" {
        "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2".parse()?
    } else {
        intent.token_in.parse()?
    };

    let token_out_addr: Address = if intent.token_out == "0x0000000000000000000000000000000000000000" {
        "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2".parse()?
    } else {
        intent.token_out.parse()?
    };

    let amount = U256::from_dec_str(&intent.amount_in)?;
    let fee = U256::from(3000u32);

    let selector = ethers::utils::id("quoteExactInputSingle(address,address,uint24,uint256,uint160)");
    let calldata = [
        &selector[..4],
        &encode(&[
            Token::Address(token_in_addr),
            Token::Address(token_out_addr),
            Token::Uint(fee),
            Token::Uint(amount),
            Token::Uint(U256::zero()),
        ])
    ].concat();

    let tx = ethers::types::TransactionRequest::new()
        .to(quoter)
        .data(calldata);

    let result = client.call(&tx.into(), None).await?;
    let amount_out = U256::from_big_endian(&result);
    
    let slippage_factor = U256::from(10000 - intent.slippage_bps);
    let min_amount_out = amount_out * slippage_factor / U256::from(10000);

    // Вычисляем цену
    let price = format!("1 token = {} tokens", amount_out.as_u128() as f64 / amount.as_u128() as f64);

    Ok(QuoteResponse {
        amount_out: amount_out.to_string(),
        min_amount_out: min_amount_out.to_string(),
        price,
        dex: "Uniswap V3".to_string(),
        route: format!("{:?} → {:?}", token_in_addr, token_out_addr),
    })
}

async fn approve_token(
    client: &Arc<SignerMiddleware<Provider<Http>, LocalWallet>>,
    token: Address,
    spender: Address,
    amount: U256,
) -> Result<()> {
    use ethers::abi::{encode, Token};

    let selector = ethers::utils::id("approve(address,uint256)");
    let calldata = [&selector[..4], &encode(&[Token::Address(spender), Token::Uint(amount)])].concat();

    let tx = ethers::types::TransactionRequest::new()
        .to(token)
        .data(calldata)
        .from(client.address())
        .gas(100000);

    let pending = client.send_transaction(tx, None).await?;
    pending.await?;
    Ok(())
}

async fn resolve_address(provider: &Provider<Http>, input: &str) -> Result<Address> {
    if let Ok(addr) = input.parse::<Address>() {
        return Ok(addr);
    }

    if input.ends_with(".eth") {
        let addr = provider.resolve_name(input).await
            .context(format!("Failed to resolve ENS: {}", input))?;
        println!("  ENS {} → {:?}", input, addr);
        return Ok(addr);
    }

    anyhow::bail!("Invalid address: {}", input)
}