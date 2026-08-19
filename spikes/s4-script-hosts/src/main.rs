//! Spike S4: prove both scripting hosts work on this platform.
//! - mlua (Lua 5.1, vendored) -- the gopher-lua replacement
//! - extism -- the WASM plugin host replacing Go .so plugins
//!
//! The extism stage needs `vendor/count_vowels.wasm` (downloaded, gitignored);
//! it reports SKIP if absent so the crate still builds/runs everywhere.

fn main() {
    lua_demo().expect("lua host failed");
    match extism_demo() {
        Ok(Some(out)) => println!("extism: count_vowels -> {out}"),
        Ok(None) => println!("extism: SKIP (vendor/count_vowels.wasm missing)"),
        Err(e) => panic!("extism host failed: {e}"),
    }
    println!("S4 PASS");
}

fn lua_demo() -> mlua::Result<()> {
    let lua = mlua::Lua::new();
    let host_add = lua.create_function(|_, (a, b): (i64, i64)| Ok(a + b))?;
    lua.globals().set("hostAdd", host_add)?;
    let script = r#"
        local sum = hostAdd(40, 2)
        local s = "wire" .. "-pod"
        return sum, s, string.upper(s), _VERSION
    "#;
    let (sum, s, upper, version): (i64, String, String, String) = lua.load(script).eval()?;
    assert_eq!(sum, 42);
    assert_eq!(s, "wire-pod");
    assert_eq!(upper, "WIRE-POD");
    println!("lua: hostAdd=42 concat={s} upper={upper} ({version})");
    assert_eq!(version, "Lua 5.1");
    Ok(())
}

fn extism_demo() -> Result<Option<String>, Box<dyn std::error::Error>> {
    let wasm_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("vendor/count_vowels.wasm");
    if !wasm_path.exists() {
        return Ok(None);
    }
    let wasm = extism::Wasm::file(&wasm_path);
    let manifest = extism::Manifest::new([wasm]);
    let mut plugin = extism::Plugin::new(&manifest, [], true)?;
    let out = plugin.call::<&str, &str>("count_vowels", "hello wire-pod")?;
    assert!(out.contains("\"count\""), "unexpected plugin output: {out}");
    Ok(Some(out.to_string()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn lua_host_works() {
        super::lua_demo().unwrap();
    }

    #[test]
    fn extism_host_works_when_vendored() {
        // Passes trivially when the wasm is absent (e.g. fresh checkout/CI);
        // exercises the real host when it is present.
        super::extism_demo().unwrap();
    }
}
