use std::fs;
use wasmer::{Store, Module, Engine, Instance, imports};
use anyhow::Result;

pub fn load(path: &str) -> Result<(Store, Instance)> {
    let wasm_bytes = fs::read(path)?;
    let engine = Engine::default();
    let mut store = Store::new(engine);
    let module = Module::new(&store, &wasm_bytes)?;
    let instance = Instance::new(&mut store, &module, &imports!{})?;
    Ok((store, instance))
}
//功能已经验证过可以正常使用