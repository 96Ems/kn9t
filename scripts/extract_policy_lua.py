import io, os, sys
sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding="utf-8")

src = open("plugins/kn9t-policy/kn9t_policy/__main__.py", encoding="utf-8").read()
marker = "UI_LUA = r'''"
start = src.index(marker) + len(marker)
end = src.index("'''", start)
lua = src[start:end]

os.makedirs("crates/kn9t-tui/tests", exist_ok=True)
out = "crates/kn9t-tui/tests/policy_ui.lua"
open(out, "w", encoding="utf-8", newline="\n").write(lua)
print(f"extracted {len(lua)} chars -> {out}")
print("defines render:", "function render(" in lua)
