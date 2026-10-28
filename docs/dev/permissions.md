# Resetting macOS permissions while developing

TCC grants are keyed to the bundle identifier (`com.drake0306.lvw`) for the bundled app, and to the binary for a bare `target/debug/lvw`. To make macOS ask again:

```bash
tccutil reset Microphone com.drake0306.lvw
tccutil reset Accessibility com.drake0306.lvw
tccutil reset ListenEvent com.drake0306.lvw      # Input Monitoring
tccutil reset All com.drake0306.lvw
```

For a debug binary started from a terminal, the grant is attributed to the terminal app; reset that instead (for example `tccutil reset Microphone com.apple.Terminal`).

Open a pane directly:

```bash
open "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone"
open "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
open "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent"
```

Symptoms: the pill shows "Microphone unavailable" (no mic grant), history rows with status `copied_only` (no Accessibility: text is on the clipboard, paste it yourself), the Right Option key does nothing while Ctrl+Shift+Space works (no Input Monitoring).

Drive the pipeline without any permission except the microphone:

```bash
LVW_DEBUG_SCRIPT="start:1500,stop:5000" ./target/debug/lvw
```
