# TUI — plan de nettoyage & d'improvement (Lua-owned)

État constaté au commit `6862af4` + working tree. Chaque point est vérifié à la
ligne citée. Objectif : `AGENTS.md` §11.1 dit « Rust renders, Lua decides ».
Aujourd'hui c'est vrai pour ~1 écran sur 8.

---

## 0. Résumé exécutif

| | lignes | Lua-scriptable ? |
|---|---|---|
| `ui/render.rs` | 4160 | 3 vues natives (`transcript`/`input`/`status`) sur 41 fonctions |
| `app.rs` | 4140 | 0 |
| `diff_viewer.rs` | 1832 | 0 — rendu **hors** de l'arbre Lua |
| overlays (7) | ~1700 | 0 |
| tool cards | ~700 | 0 |
| `render_welcome` | 236 | 0 |
| `lua/*` | ~3600 | — |

Trois classes de problèmes, par gravité :

1. **API Lua annoncée mais morte** (§1) — le pire : `default_tui.lua` documente
   `kn9t.get_messages`, `kn9t.get_tools`, `kn9t.http` ; aucun n'existe au
   runtime. `render_status()` est défini, exporté, testé… et jamais appelé.
2. **Vestiges de l'ancienne UI non-customizable** (§2) — `Sidebar`,
   `LayoutState`, `right_sidebar`, `Action::ToggleRight`, `sidebar_area`,
   `toggle_sidebar` : ~200 lignes de state mort qui *mentent* (le palette
   command « Toggle Sidebar » ne fait plus rien).
3. **Surfaces encore hardcodées** (§3) — tool cards, `/diff`, overlays,
   welcome. Le vocabulaire de widgets est trop pauvre pour les porter (§4).

---

## 1. API Lua morte ou fantôme — À CORRIGER EN PREMIER

C'est la priorité absolue : un utilisateur qui lit l'en-tête de
`assets/default_tui.lua` écrit du code contre une API qui n'existe pas, et
échoue silencieusement.

### 1.1 `render_status()` n'est jamais appelé — le status bar Lua est décoratif

- `lua/mod.rs:459` `call_status_bar()` → 0 appelant hors test
  (`lua/default_config.rs:215`).
- `ui/render.rs:284` `"status" => render_status(f, app, area, theme)` → part
  directement sur le Rust hardcodé `ui/render.rs:1232-1279` (`format!("{} | ${:.4} | …")`).
- `assets/default_tui.lua:337-399` construit 60 lignes de segments colorés
  (barre de mix de messages, gauge de contexte, coût, t/s) **jetées à la poubelle**.
- `StatusBarResult` / `StatusSegment` (`lua/mod.rs:500-512`) n'ont aucun
  consommateur.

**Fix** : dans `render_native_view`, pour `"status"` :
`runtime.call_status_bar()` d'abord, rendu des segments via `parse_color`, et
`render_status` Rust conservé uniquement comme fallback `None`. ~30 lignes.

### 1.2 `kn9t.get_messages` / `kn9t.get_tools` n'existent pas au runtime

- `lua/state.rs:205` `install_lazy_accessors()` → appelé **uniquement dans ses
  propres tests** (`:377`, `:402`, `:425`).
- Pourtant documenté comme API publique : `assets/default_tui.lua:15-17`,
  `lua/state.rs:15`.
- Conséquence : `kn9t.get_messages(1, 10)` → `attempt to call a nil value`,
  donc `UiOutcome::Failed`, donc écran d'erreur rouge. Pour une API documentée.

**Fix** : appeler `install_lazy_accessors` par frame (ou au moins après chaque
mutation transcript) depuis `render_chat`, avec `LazyData` construit à partir
d'`app`. Le `Arc<LazyData>` est déjà conçu pour ça (`lua/state.rs:183-199`).
Attention : `LazyData` copie le contenu des messages — le construire **lazy**
(closure sur un snapshot `Arc`) et pas eager, sinon on retombe sur les 3.5 ms/frame
que `StateSnapshot` avait éliminés (`AGENTS.md` §11.1).

### 1.3 `kn9t.http` est un no-op silencieux, et son whitelist est fausse

- `lua/mod.rs:379` `set_http_config()` → 0 appelant ⇒ `_kn9t_http_config` est
  toujours `nil` ⇒ tout appel renvoie `nil, "http not configured"`.
- `lua/mod.rs:391` `drain_http_requests()` → 0 appelant ⇒ même si configuré,
  `_kn9t_http_pending` grossit sans fin, aucune requête n'est jamais émise.
  C'est une **fuite mémoire** en plus d'un no-op.
- Whitelist obsolète (`lua/http.rs:13-18`) : `POST /abort` n'existe pas
  (`API.md` §2 : `POST /session/{id}/abort`). Et rien d'utile n'est autorisé :
  ni `/tools`, `/models`, `/cost`, `/budget`, `/pref/{key}`, `/session/{id}/export`.

**Décision à prendre** (je recommande B) :
- **A** — brancher : appeler `set_http_config` dans `init_lua`, drainer dans
  `run()` à côté de `process_lua_panels`, exécuter sur un thread, réinjecter la
  réponse via un `Event::LuaHttpResponse`. ~150 lignes + un modèle de callback
  asynchrone en Lua.
- **B** — supprimer `lua/http.rs` (322 l.) et remplacer par des *actions*
  déclaratives : `kn9t.action("refresh_tools")`, `kn9t.action("approve", …)`.
  Rust fait l'I/O, Lua exprime l'intention. Cohérent avec `kn9t.action` qui
  marche déjà (`lua/keymap.rs:109-121`), et supprime la surface de sécurité.

### 1.4 Panels : les touches ne leur arrivent jamais

- `lua/mod.rs:373` `handle_panel_key()` → 0 appelant.
- `lua/panels.rs:51,245` `on_key` est parsé, stocké, et jamais invoqué.
- `set_focus` / `focused` / `handle_focus` / `handle_blur` : même sort.

### 1.5 Le widget `input` Lua est toujours vide

- `app.lua_input_states` (`app.rs:458`) est initialisé (`:524`) puis **jamais
  écrit** — seulement lu dans `render_widget`.
- ⇒ `{type="input", id="x"}` affiche éternellement son placeholder.

### 1.6 Autres API sans appelant

| symbole | fichier |
|---|---|
| `call_string` | `lua/mod.rs:199` |
| `call_table` | `lua/mod.rs:229` |
| `get_global` | `lua/mod.rs:258` |
| `state_number` | `lua/state.rs:287` |
| `create_sandbox` (re-export) | `lua/mod.rs:40` |

À supprimer ou à brancher, mais pas à laisser dans un `pub` d'un module documenté.

---

## 2. Vestiges de l'ancienne UI — à supprimer

### 2.1 `ui/layout.rs` : `Sidebar` + `LayoutState` (à supprimer entièrement)

Le module dit lui-même (`ui/layout.rs:3`) que le layout est en Lua. Ce qui reste
est un modèle de sidebar **fictif** :

- `RIGHT_EXPANDED = 24` (`:56`) : largeur codée en dur, alors que
  `default_tui.lua:27` utilise `SIDEBAR_WIDTH = 34`. `input_height_for`
  (`:99`) soustrait donc 24 colonnes arbitraires ⇒ `ctx.input_height` publié à
  Lua est **faux** dès que la sidebar Lua fait une autre largeur ou est cachée
  (F5, `default_tui.lua:403`). C'est un bug de curseur latent, pas juste du
  code mort.
- `toggle_right` (`:34`), `expand_right` (`:42`), `collapse_right` (`:48`) :
  0 appelant.
- `effective_right_state`, `width`, `MIN_WIDTH`, `MIN_CENTER` : ne servent qu'à
  `input_height_for`.
- Les 3 tests (`:156-187`) valident ce modèle fictif.

**Fix** : garder `calculate_input_height(input, width, max_lines)`, la déplacer
(p.ex. `ui/input_metrics.rs` ou dans `render.rs`), et l'appeler avec la
**largeur réelle** — c'est-à-dire : soit Lua déclare la largeur du panneau
d'input, soit on résout `collect_natives` **avant** de publier le contexte.
Solution propre : passer à un `render_ui` en **deux passes** (§4.4).
Supprimer `ui/layout.rs` et `pub mod layout` (`ui/mod.rs:3`).

### 2.2 Le state sidebar dans `app.rs` / `config.rs` / `keybind.rs`

| élément | ref | constat |
|---|---|---|
| `App::layout` | `app.rs:339`, `:464-472` | ne pilote plus rien |
| `App::sidebar_area` | `app.rs:439` | mis à `None` en début de frame (`render.rs:42`) et **jamais réécrit** ⇒ `is_sidebar_click` (`app.rs:2751`) toujours `false` ⇒ le refresh-tools au clic (`app.rs:2771-2783`) est mort |
| `Config::right_sidebar` | `config.rs:17,28,92` | lu une fois, jamais consulté |
| `TuiSection::left_sidebar` | `config.rs:155` | `#[allow(dead_code)]` explicite |
| `Action::ToggleRight` | `keybind.rs:33,279` | aucun bras dans `execute_action` ⇒ `kn9t.action("toggle_right")` est un no-op (et c'est testé comme tel : `lua/keymap.rs:347`) |
| `Action::ToggleLeft` | `keybind.rs:32` → `app.rs:2404` | nom mensonger : ouvre le picker de session |
| palette `toggle_sidebar` | `command_palette.rs:175`, `app.rs:3917-3927` | mute `self.layout`, donc ne fait rien de visible |
| `lib.rs:5` | | « R-TUI-030: 3-column layout with collapsible sidebars » — faux |

**Fix** : supprimer le tout. Le toggle de sidebar est déjà en Lua
(`default_tui.lua:403`, `SIDEBAR_VISIBLE`). Renommer `ToggleLeft` →
`SessionList` (l'action `session_list` existe déjà côté palette,
`command_palette.rs:110`). Corriger `lib.rs:5`.

### 2.3 Modules 100 % morts

- `hyperlinks.rs` (235 l.) — 0 référence externe.
- `latex.rs` (574 l.) — 0 référence externe.

Les deux sont marqués DONE dans `docs/TUI_IMPROVEMENTS.md` §2.5/2.6 alors qu'ils
ne sont **pas branchés**. Soit on les câble dans `markdown.rs`, soit on les
supprime. 809 lignes.

### 2.4 Divers

- `reducer.rs:450-451,1473-1476,1535` : reliquats de `declare_page` /
  `write_placeholder` / `clear_page` (API supprimée, cf. `AGENTS.md` §14) —
  ne survivent que dans un test qui vérifie qu'on les ignore. OK à garder comme
  test de régression, mais le commentaire `:450` doit dire « removed ».
- `app.rs:977-1140` : le corps de `run()` duplique intégralement le `match event`
  dans la boucle `event_loop.drain()` (~50 lignes copiées-collées, y compris le
  message « reconnecting »). Extraire `fn handle_event(&mut self, ev, tx)`.
- `ui/render.rs:83` : `Widget::Native` doc dit « Known views: transcript, input,
  status, **tools**, **spinner** » (`lua/widgets.rs:83`) — `tools` et `spinner`
  n'existent pas (`render.rs:263-288`). Un `{type="native", view="tools"}` log
  une erreur et laisse un trou.

---

## 3. Surfaces à rendre Lua-scriptables

Par ordre de rapport valeur/coût.

### 3.1 Tool cards (~700 l., `render.rs:2486-2960`)

Le point que tu cites. Aujourd'hui :
- `get_tool_display_mode` (`:2486`) hardcode `"edit"|"write" → Diff`,
  `"read" → Summary`, `"bash" → Streaming`, reste `→ Output`. Un tool de plugin
  ne peut **pas** choisir son rendu.
- `TOOL_CARD_RIGHT_MARGIN = 20` (`:23`), `TOOL_CARD_BG = Rgb(30,33,39)` (`:2471`),
  `TOOL_OUTPUT_*_VISIBLE_LINES` (`:2465`,`:2468`) : magie non configurable.
- `reconstruct_diff_from_args` (`:2736`), `get_lang_from_args` (`:2963`),
  `bash_highlight_spans` (`:3089`), `approval_reason` (`:3024`) : de la *policy*
  par nom de tool, en Rust.

**Design proposé** — hook Lua par carte, mécanisme en Rust :

```lua
-- optionnel ; absent => défaut built-in
function render_tool(tool)
  -- tool = {name, args, status, output, progress_lines, expanded, scroll, call_id}
  if tool.name == "bash" then
    return { type = "box", title = " $ "..tool.args, border = true,
             child = { type = "text", content = tool.output, syntax = "bash" } }
  end
end
```

- Rust garde : parse du diff, coloration syntaxique, scroll, cache
  (`render_cache.rs`), hit-testing.
- Lua choisit : quel contenu, quel mode, quelles couleurs, quel titre.
- Le défaut actuel devient du Lua dans `default_tui.lua` (table
  `TOOL_MODES = { edit="diff", read="summary", bash="streaming" }`), donc
  éditable sans recompiler — c'est exactement ce que `get_tool_display_mode`
  interdit aujourd'hui.
- Prérequis : `kn9t.get_messages` vivant (§1.2), et widgets enrichis (§4).
- Coût : ~250 l. de Rust en moins, +120 l. de Lua, +1 hook.

### 3.2 `/diff` (`diff_viewer.rs` 1832 l.)

Le viewer est **hors de l'arbre Lua** : `render_chat_overlays`
(`render.rs:122-136`) le dessine en dur, centré, `width.min(100)`,
`height-4`, `y+2`. Lua ne peut ni le placer, ni le styler, ni le remplacer.
Les touches sont hardcodées dans `app.rs:1496-1568` (`j`/`k`/`[`/`]`/`u`/`f`/
`c`/`n`/`p`/`b`) — non rebindables, alors que `kn9t.map` existe.

**Étapes** :
1. `{type="native", view="diff"}` : le viewer devient une vue native placée par
   Lua (comme `transcript`). Supprime le chemin overlay spécial (`render.rs:124-136`).
2. Exposer l'état à Lua : `kn9t.state.diff = {open, files=[{path,status,adds,dels}],
   current_file, cursor_line, split_mode, fullscreen, comments=N}` — un
   `DiffSnapshot` sur le modèle de `StateSnapshot` (borné : pas les hunks).
3. Actions : `diff_next_hunk`, `diff_prev_hunk`, `diff_next_file`,
   `diff_toggle_split`, `diff_toggle_tree`, `diff_comment`, `diff_close` dans
   `parse_action` (`keybind.rs:264`) ⇒ tout rebindable via `kn9t.map`, et le
   bloc de `handle_overlay_key` disparaît.
4. `default_tui.lua` fournit les binds actuels ⇒ zéro régression UX,
   customisation totale.
5. Le file-tree (`toggle_file_tree`, `:143`) devient un simple panneau Lua
   `{type="list"}` alimenté par `kn9t.state.diff.files` ⇒ on supprime le rendu
   dédié dans `diff_viewer.rs:525-788`.

Gain : ~400 l. de rendu en moins, `/diff` entièrement thémable.

### 3.3 Overlays (~1700 l.)

`render_model_select` (213), `render_session_select` (217),
`render_tools_manager` (212), `render_command_palette` (177),
`render_overlay`/approval (203 + 200 de `approval_body_lines`),
`render_interaction_overlay` (686).

Tous suivent le **même patron** : liste filtrable + curseur + boîte centrée. Ce
patron est déjà exprimable en widgets (`box` + `list` + `input`) — il manque
juste (a) `lua_input_states` branché (§1.5), (b) le `on_key` des panels branché
(§1.4), (c) un widget `list` avec scroll.

**Proposition** : une seule primitive Rust `Overlay::Lua { id, state }`, et
`default_tui.lua` définit `render_overlay(id, state)`. `model_select`,
`session_select`, `tools_manager`, `command_palette` deviennent 4 fonctions Lua
d'~25 lignes chacune, alimentées par `kn9t.state.models` / `.sessions` / `.tools`.

Cas à **garder en Rust** : `Interaction` (686 l.) et `Approval`. Ce sont des
chemins de sécurité/contrat (`POST /approve`, `POST /ui-respond`) : un bug Lua
ne doit pas pouvoir empêcher un refus ou fabriquer une approbation. Les rendre
scriptables *en apparence seulement* (`render_approval` peut styler, mais les
touches y/n/a et l'envoi restent Rust).

### 3.4 `Screen::Welcome` (236 l., `render.rs:335-570`)

Écran entièrement hardcodé, en dehors de `render_ui`. Devrait être
`function render_welcome(width, height)` avec fallback. Cas facile, valeur
surtout cosmétique (branding).

### 3.5 Le mécanisme « panels » double `render_ui`

`lua/panels.rs` (595 l.) + `compute_panel_area` (`render.rs:224-256`)
fournissent un second système de placement (`position = "left"|"right"|"top"|
"bottom"|flottant`), avec son propre registre, ses propres commandes
(`show/hide/toggle/focus`) et son propre pipeline de drain
(`process_pending_panels`, `process_panel_commands`).

Or `render_ui` peut déjà tout placer, et `default_tui.lua` n'utilise **aucun**
panel. Deux mécanismes pour un besoin ⇒ celui qui n'est pas utilisé est celui
qui pourrit (d'où §1.4 : `on_key` mort).

**Décision** : garder les panels **uniquement** pour ce que `render_ui` ne peut
pas faire — les **flottants au-dessus** de l'arbre (popups, toasts). Supprimer
`position = left/right/top/bottom` (redondant avec `split`), supprimer
`by_position` (`panels.rs:211`, 0 appelant), et brancher `on_key`. Sinon
supprimer le module entier et exposer un `{type="float", x, y, w, h, child=…}`
dans le vocabulaire de widgets — plus simple, un seul modèle mental.

---

## 4. Le vocabulaire de widgets est le vrai facteur limitant

Impossible de porter §3 avec l'existant. `lua/widgets.rs` offre :
`text`(+markdown/syntax/wrap) · `box`(title/border) · `list`(items/selected) ·
`input`(mort) · `split`(fixed/percent/flex) · `native` · `plugin`.

Manques bloquants :

| manque | bloque | où |
|---|---|---|
| pas de spans multi-couleurs dans un `text` | status bar Lua, tool headers, tout ce qui mêle 2 couleurs sur une ligne | `widgets.rs:42-54` : un seul `fg` par nœud |
| `list` sans scroll ni viewport | model/session/tools pickers | `widgets.rs:63-67` |
| pas de padding/margin | tout le layout fin (aujourd'hui : `string.rep(" ")` à la main, `default_tui.lua:75-84`) | — |
| pas de couleur/style de bordure | `box` ne peut pas signaler un état (focus, erreur) | `widgets.rs:56-61` |
| pas d'alignement (center/right) | titres, gauges | — |
| pas de `gauge`/`bar` | `default_tui.lua:62-66` simule avec `#` et `-` | — |
| `parse_color` sans les 8 couleurs vives | `lightgreen`/`lightred` utilisés dans `default_tui.lua:43-48` → `None` → couleur du thème. **Les couleurs du built-in sont silencieusement ignorées.** | `widgets.rs:298-318` |
| pas d'accès au thème depuis Lua | Lua ne peut pas écrire `fg = theme.user` | `theme.rs:13-35` non exposé |
| `size` sur `child` et non sur le parent | asymétrie qui piège (`widgets.rs:249`) | — |

**Bug concret à corriger tout de suite** : `parse_color` (`widgets.rs:298`) ne
connaît ni `lightgreen`, ni `lightred`, ni `lightblue`… alors que
`default_tui.lua:43-45` les utilise pour `ok`/`danger`. Toute la sémantique
couleur du built-in (context warn/danger) est morte. ~10 lignes.

### 4.1 Vocabulaire cible

```lua
{ type="text", spans={ {text="ctx ", fg="gray"}, {text="87%", fg="#ff5555", bold=true} } }
{ type="box", title=…, border="rounded"|"thick"|"none", border_fg=…, padding={1,2} }
{ type="list", items=…, selected=…, offset=…, height_hint=…, on_key=… }
{ type="gauge", frac=0.62, fg=…, bg=…, label="62%" }
{ type="float", x=…, y=…, w=…, h=…, child=… }
{ type="spacer", size={flex=1} }
kn9t.theme  -- { fg, muted, primary, user, assistant, tool, error, warning, success, … }
```

### 4.2 Deux passes pour `render_ui`

Le bug `input_height` (§2.1) et la vue `diff` (§3.2) viennent tous les deux du
même défaut : Lua a besoin de la géométrie *avant* de la déclarer.

Passe 1 : `render_ui(w, h)` → arbre → `collect_natives` donne les rects réels.
Passe 2 : publier `kn9t.layout = { transcript={x,y,w,h}, input={…}, … }` puis
appeler les fonctions de contenu (`render_status`, `render_tool`) qui, elles,
connaissent leur largeur.

Bonus : `sidebar_area`/`transcript_area` (§2.2) deviennent inutiles —
`collect_natives` **est** la source de vérité pour le hit-testing.

---

## 5. Manques côté serveur (feedback API, `AGENTS.md` §11)

1. **`ctx_window` n'est pas plumbé** — déjà noté `TRACKING.md` B3.
   `GET /models` le renvoie (`routes/models.rs:22`), `ModelEntry`
   (`model_selector.rs:9-13`) le jette, donc `default_tui.lua:33` hardcode
   `CONTEXT_WINDOW = 200000`. La gauge de contexte du built-in est **fausse**
   sur tout modèle ≠ 200k. Fix : ajouter `ctx_window`/`max_out` à `ModelEntry`
   et les publier dans `kn9t.context`. Petit et à fort impact.
2. **Pas de budget de compaction exposé** — la vraie question de l'utilisateur
   est « quand vais-je être compacté ». Le seuil est côté serveur ; l'exposer
   (`GET /session/{id}` → `compact_threshold`, `tokens_estimate`) rend la gauge
   honnête au lieu d'estimée.
3. **Aucune API pour la disposition/le thème partagés** — pas bloquant, mais si
   un jour un plugin veut suggérer un layout, le mécanisme existe déjà
   (`ui_register_lua`, `AGENTS.md` §14). Ne rien ajouter avant un besoin réel.
4. **Whitelist HTTP à réaligner ou supprimer** (§1.3).

---

## 6. Ordre d'exécution proposé

**Phase A — arrêter de mentir (petit, immédiat)**
1. Brancher `render_status()` (§1.1).
2. `parse_color` : couleurs vives (§4, bug concret).
3. Supprimer `ui/layout.rs`/`Sidebar`/`right_sidebar`/`ToggleRight`/
   `sidebar_area`/palette `toggle_sidebar` ; renommer `ToggleLeft` (§2.1, §2.2).
4. Corriger `lib.rs:5` et la doc `Native` (`widgets.rs:83`).
5. Décider et appliquer §1.3 (je recommande : supprimer `lua/http.rs`).
6. Supprimer `hyperlinks.rs` + `latex.rs` ou les câbler (§2.3).
7. Extraire `handle_event` de `run()` (§2.4).

→ ~1200 lignes en moins, 0 régression fonctionnelle, la doc redevient vraie.

**Phase B — rendre l'API Lua réelle**
8. `install_lazy_accessors` par frame, en lazy (§1.2).
9. `lua_input_states` + `on_key` des panels branchés (§1.5, §1.4).
10. Vocabulaire de widgets : `spans`, `padding`, `border_fg`, `gauge`,
    `list`+scroll, `kn9t.theme` (§4.1).
11. `render_ui` en deux passes + `kn9t.layout` (§4.2), suppression de
    `input_height_for`.
12. `ctx_window` plumbé (§5.1).

**Phase C — porter les surfaces**
13. `render_tool` hook + défauts en Lua (§3.1).
14. `/diff` en vue native + actions rebindables (§3.2).
15. Overlays de sélection en Lua ; approval/interaction restent Rust (§3.3).
16. `render_welcome` en Lua (§3.4).
17. Trancher panels vs `float` (§3.5).

**Cible** : `render.rs` ~1200 l. (transcript + input + approval/interaction +
primitives), `app.rs` ~2500 l., `default_tui.lua` ~700 l.

---

## 7. Filet de sécurité (à faire avant la phase C)

`crates/kn9t-tui/tests/acceptance.rs` ne contient **qu'un** test
(`tui_no_kn9t_deps`). Porter 1700 lignes d'overlays vers Lua sans tests est un
pari.

Minimum avant phase C :
- **Snapshot tests** sur `Buffer` ratatui : rendre `render_ui` sur un `App`
  synthétique en 100×30 et comparer à un buffer attendu (déjà fait par
  `bench.rs` pour la perf ; réutiliser le générateur de transcript).
- **Test de contrat** : pour chaque symbole documenté dans l'en-tête de
  `default_tui.lua`, asserter qu'il est non-`nil` après `load_builtin` +
  `update_state` + `install_*`. Ce test seul aurait attrapé §1.1, §1.2 et §1.3.
- **Test de non-régression du budget par frame** : `StateSnapshot::collect` +
  `render_ui` < 0.5 ms sur 500 messages (garde-fou contre le retour du deep-copy).
