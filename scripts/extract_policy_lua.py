# Extracts the Lua UI embedded in the kn9t-policy plugin into a TUI test fixture.
#
# The plugin source lives in the kn9t-plugins repo, not here:
#   https://github.com/96Ems/kn9t-plugins/tree/main/kn9t-policy
# Clone it next to this repo (default) or point KN9T_PLUGINS at your checkout.
import io, os, sys
sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding="utf-8")

plugins = os.environ.get("KN9T_PLUGINS", os.path.join("..", "kn9t-plugins"))
src = open(os.path.join(plugins, "kn9t-policy/kn9t_policy/__main__.py"), encoding="utf-8").read()
marker = "UI_LUA = r'''"
start = src.index(marker) + len(marker)
end = src.index("'''", start)
lua = src[start:end]

os.makedirs("crates/kn9t-tui/tests", exist_ok=True)
out = "crates/kn9t-tui/tests/policy_ui.lua"
open(out, "w", encoding="utf-8", newline="\n").write(lua)
print(f"extracted {len(lua)} chars -> {out}")
print("defines render:", "function render(" in lua)
