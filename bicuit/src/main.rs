use eyre::eyre;

fn create_biscuit(identifier: String) -> eyre::Result<String> {
    let root_key_bytes_hex = include_str!("../../assets/private.key");
    let root_key = biscuit_auth::PrivateKey::from_bytes_hex(root_key_bytes_hex)?;
    let keypair = biscuit_auth::KeyPair::from(&root_key);

    let biscuit_builder = biscuit_auth::macros::biscuit!(r#"id({id})"#, id = identifier);

    let token = biscuit_builder.build(&keypair)?;

    let token_b64 = token.to_base64()?;

    Ok(token_b64)
}

fn main() -> eyre::Result<()> {
    let mut args = std::env::args();
    args.next().ok_or(eyre!("Unable to get program name"))?;
    let identifier = args.next().ok_or(eyre!("Unable to get identifier value"))?;

    println!("{}", create_biscuit(identifier)?);

    Ok(())
}
