# Extracts the Lua UI embedded in the kn9t-compactor plugin into a TUI test fixture.
#
# The plugin source lives in the kn9t-plugins repo, not here:
#   https://github.com/96Ems/kn9t-plugins/tree/main/kn9t-compactor
# Clone it next to this repo (default) or point KN9T_PLUGINS at your checkout.
import io, os, sys
sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding="utf-8")

plugins = os.environ.get("KN9T_PLUGINS", os.path.join("..", "kn9t-plugins"))
src = open(os.path.join(plugins, "kn9t-compactor/src/main.ts"), encoding="utf-8").read()
start = src.index("const COMPACTOR_LUA = ") + len("const COMPACTOR_LUA = ")
assert src[start] == "`", "expected a template literal"
end = src.index("`;", start + 1)
lua = src[start + 1 : end]

os.makedirs("crates/kn9t-tui/tests", exist_ok=True)
out = "crates/kn9t-tui/tests/compactor_ui.lua"
open(out, "w", encoding="utf-8", newline="\n").write(lua)
print(f"extracted {len(lua)} chars -> {out}")
print("defines render:", "function render(" in lua)
