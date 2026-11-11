# Native client behavior

The authoritative brief is the user's selection of a native Rust interface and
backend. `docs/ARCHITECTURE.md`, core Settings/Store APIs and plugin API v1 govern
data and extension boundaries. This contract governs observable native behavior.

| Flow | Trigger | Pending | Success | Failure and recovery |
| --- | --- | --- | --- | --- |
| Conversation | New conversation or saved row | Local load | Select it and focus composer | Keep prior session if draft cannot save |
| Send | Send or Ctrl+Enter | Stable Stop action, streaming text; preserve next draft | Save completed reply | Preserve user message/partial text; Retry reply is explicit |
| Stop | Stop | Stopping status | Cancel HTTP request and save partial reply | Keep controls usable and report error |
| Draft | Edit composer | Debounced local save | Restore on switching/relaunch | Preserve text and block close if saving fails |
| Settings | Settings/model button | App-owned modal; masked API key | Save valid non-secret settings, retain key in memory | Inline validation; leave fields and modal intact |
| Install extension | Choose local folder | Show installation progress | Refresh installed list | Keep folder picker cancellable; fixed inline error |
| Run extension | Explicit command | Bounded background worker | Replace unchanged draft with result | Preserve draft on trap/limit/error or stale result |
| Remove extension | Remove | Confirmation focused on Keep extension | Refresh installed list | Keep entry and show failure |
| Tool connection | Tool connections / Connect | Native progress, responsive window | Show connected tool count | Fixed failure; reconnect after timeout/cancel |
| Tool call | Model requests an action | Save plan, then native approval modal | Allow once executes and saves result | Deny/Stop; failed save prevents dispatch |
| Tool activity | Tool activity | Local saved trace | Latest 50 calls with expandable previews | Unknown outcomes remain explicit; no replay action |
| Close | Window close | Flush draft and cancel reply | Save partial reply and exit | Remain open if draft save fails |

GTK owns controls, accessibility roles, modal focus/keyboard behavior, file
selection, native list interaction and input methods. Enter inserts a newline;
Ctrl+Enter is an explicit send shortcut. The composer remains editable during a
reply, but sending, changing sessions and changing connection settings wait until
the active request settles. There is one active reply per app instance.

Messages use bounded pages of at most 50 selectable native labels, with explicit
Earlier messages/Latest messages controls and a range indicator. Streaming
updates text in display batches; final Markdown renders without HTML execution.
Auto-scroll follows the bottom unless the user has scrolled up. Retry does not
silently send a second paid request; only the explicit button starts it.

The native profile is independent of existing Harness data. Conversations/drafts
are private local files; each model request transmits the selected conversation
to the configured provider. API keys are masked, held only in memory and omitted
from saved settings, logs and fixed user-facing failures. The current preview
implements chat and explicit text extensions, not an autonomous tool executor.

Plugin API v1 receives only the draft supplied by the user. No filesystem,
network, subprocess or WASI imports are provided. Installation is local and
reversible; existing Harness JavaScript/React packages use a different contract.

Locale is English; arbitrary Unicode content is preserved. Window geometry,
lists and dialogs must remain usable at 640×480. Controls use visible native
focus, text labels or explicit accessible names. Status and error text remains
available until the next relevant action.
