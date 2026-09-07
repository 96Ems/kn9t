import io, os, re, sys
sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding="utf-8")

src = open("plugins/kn9t-ask-user/src/main.ts", encoding="utf-8").read()
start = src.index("const UI_LUA = ") + len("const UI_LUA = ")
assert src[start] == "`", "expected a template literal"
end = src.index("`;", start + 1)
lua = src[start + 1 : end]

os.makedirs("crates/kn9t-tui/tests", exist_ok=True)
out = "crates/kn9t-tui/tests/ask_user_ui.lua"
open(out, "w", encoding="utf-8", newline="\n").write(lua)
print(f"extracted {len(lua)} chars -> {out}")
print("defines render:", "function render(" in lua)
