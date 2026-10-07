use whitebox_core::{self, init};
fn main() -> anyhow::Result<()> {
    init()?;
    println!("Hello, world!");
    anyhow::Ok(())
}
