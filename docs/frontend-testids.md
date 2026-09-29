# Test ids in the web interface

Every authored element of the web interface carries a `data-testid` attribute. Anyone inspecting an element in the browser can read what it is for and find where it comes from, and tests can select it without depending on classes, ids or copy. The ids ship in the served pages on purpose. Nothing strips them, and nothing that would strip them should be added.

## The scheme

An id has the form `<area>-<path>-<role>[-<key>]`, in lowercase kebab-case for every part the source spells out.

- The area is one fixed prefix per page or surface, listed below.
- The path names the block, in as many words as it needs, such as `nav-log` or `kg-openai-key`.
- The role says what the element does. Interactive roles are `btn`, `link`, `input`, `select`, `textarea`, `checkbox`, `radio`, `form` and `label`. Content roles are `heading`, `eyebrow`, `lead`, `text`, `img`, `icon`, `chip`, `pill`, `badge`, `note`, `row` and `item`. Wrapper roles are `root`, `header`, `body`, `footer`, `inner`, `row`, `col`, `group`, `list`, `actions` and `media`.
- The key appears only on elements a script builds in a loop or builds once per container, so that every rendered id is unique. It is the natural value from the data, written exactly as the data has it, so it can contain capitals or spaces. Examples are a robot serial, an intent name, an image id, a face id, an SSID, a log component, a map content type, a local ISO date or a container id. Log rows have no natural key, so they use their position in the page's log buffer, written `i<n>`.
- A static `<option>` is named from its value as `<select path>-<value>-item`, for example `log-level-debug-item`.

## Areas

| Prefix | Where |
|---|---|
| `home` | The shell of `index.html`, its navigation tiles, the battery cards built by `js/battery.js` and the status strip built by `js/main.js` |
| `intents` | The custom intents section of `index.html` and the intent forms `js/main.js` builds |
| `bot-setup` | The bot setup section of `index.html`, `js/ble.js` and `js/ssh.js` |
| `log` | The log section of `index.html` and the rows `js/main.js` builds |
| `version` | The version section of `index.html` |
| `ui-settings` | The UI customizer section of `index.html` |
| `status` | The paragraphs `displayMessage` and `displayError` in `js/main.js` write on any page, keyed by the container's id |
| `server-settings` | `setup.html` |
| `setup` | `initial.html` and `js/initial.js` |
| `bot-picker` | `sdkapp/index.html` and `sdkapp/js/auth.js` |
| `sdk-dashboard` | The Vector Brain card of `sdkapp/settings.html`, the frame of its settings drawer, and `sdkapp/js/vectorbrain.js` |
| `bot-settings` | The settings sections inside that drawer, `sdkapp/js/main.js`, `sdkapp/js/faces.js` and `sdkapp/js/heyvector.js` |
| `bot-control` | `sdkapp/control.html` and `sdkapp/js/control.js` |
| `navmap` | `crates/wirepod-server/src/navmap/navmap.html` |

## Adding markup

In static HTML the attribute goes right after the tag name. In a script it goes on a new `setAttribute("data-testid", ...)` line after the element is created, or inside the template string that builds it. Adding an id never changes anything else on the line, and static ids stay unique within their file.

Formatting and document tags carry no id: `<br>`, `<hr>`, `<html>`, `<head>` and everything in it, `<script>`, `<style>`, and the hidden `hiddenFrame` iframe. An SVG icon is tagged on its `<svg>` element, and the shapes inside it are left alone. Elements a script creates but never attaches to the page are left untagged.

Third-party code is not tagged: `sdkapp/js/iro.min.js` and the colour picker it builds, Chart.js, and the Font Awesome kit.

The files under `assets/webroot/` are stored with CRLF line endings, and edits keep them. `navmap.html` uses LF.
