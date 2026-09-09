//! Unit tests for lua/sandbox — extracted from src/lua/sandbox.rs
#![allow(clippy::unwrap_used)]

use kn9t_tui::lua::create_sandbox;
use mlua::{Table, Value};

#[test]
fn test_io_removed() {
    let lua = create_sandbox().unwrap();
    let io: Value = lua.globals().get("io").unwrap();
    assert!(matches!(io, Value::Nil));
}

#[test]
fn test_debug_removed() {
    let lua = create_sandbox().unwrap();
    let debug: Value = lua.globals().get("debug").unwrap();
    assert!(matches!(debug, Value::Nil));
}

#[test]
fn test_package_removed() {
    let lua = create_sandbox().unwrap();
    let package: Value = lua.globals().get("package").unwrap();
    assert!(matches!(package, Value::Nil));
}

#[test]
fn test_loadfile_removed() {
    let lua = create_sandbox().unwrap();
    let loadfile: Value = lua.globals().get("loadfile").unwrap();
    assert!(matches!(loadfile, Value::Nil));
}

#[test]
fn test_dofile_removed() {
    let lua = create_sandbox().unwrap();
    let dofile: Value = lua.globals().get("dofile").unwrap();
    assert!(matches!(dofile, Value::Nil));
}

#[test]
fn test_os_execute_removed() {
    let lua = create_sandbox().unwrap();
    let os: Table = lua.globals().get("os").unwrap();
    let execute: Value = os.get("execute").unwrap();
    assert!(matches!(execute, Value::Nil));
}

#[test]
fn test_os_time_available() {
    let lua = create_sandbox().unwrap();
    let result: i64 = lua.load("return os.time()").eval().unwrap();
    assert!(result > 0);
}

#[test]
fn test_os_date_available() {
    let lua = create_sandbox().unwrap();
    let result: String = lua.load("return os.date('%Y')").eval().unwrap();
    assert_eq!(result.len(), 4); // Year is 4 digits
}

#[test]
fn test_safe_functions_available() {
    let lua = create_sandbox().unwrap();

    // Test pairs
    let result: i64 = lua
        .load("local sum = 0; for k, v in pairs({1, 2, 3}) do sum = sum + v end; return sum")
        .eval()
        .unwrap();
    assert_eq!(result, 6);

    // Test string
    let result: String = lua.load("return string.upper('hello')").eval().unwrap();
    assert_eq!(result, "HELLO");

    // Test math
    let result: f64 = lua.load("return math.floor(3.7)").eval().unwrap();
    assert_eq!(result, 3.0);

    // Test table
    let result: i64 = lua
        .load("local t = {3, 1, 2}; table.sort(t); return t[1]")
        .eval()
        .unwrap();
    assert_eq!(result, 1);
}

#[test]
fn test_load_string_works() {
    let lua = create_sandbox().unwrap();
    let result: i64 = lua
        .load("local f = load('return 42'); return f()")
        .eval()
        .unwrap();
    assert_eq!(result, 42);
}

#[test]
fn test_load_bytecode_blocked() {
    let lua = create_sandbox().unwrap();
    // Bytecode starts with \x1bLua
    let result = lua.load("return load('\\x1bLuaXXX')").eval::<Value>();
    assert!(result.is_err());
}

#[test]
fn test_collectgarbage_collect_allowed() {
    let lua = create_sandbox().unwrap();
    // Should not error
    let _: Value = lua.load("collectgarbage('collect')").eval().unwrap();
}

#[test]
fn test_collectgarbage_stop_blocked() {
    let lua = create_sandbox().unwrap();
    let result = lua.load("collectgarbage('stop')").eval::<Value>();
    assert!(result.is_err());
}
