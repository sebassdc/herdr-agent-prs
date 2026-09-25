# VHS script for the README demo. Run via demo/record.sh (sets up the
# isolated Herdr config and fixture data).
Output assets/demo.gif

Require herdr
Set Shell "bash"
Set FontSize 15
Set Width 1400
Set Height 720
Set Padding 12
Set Theme "Catppuccin Mocha"
Set TypingSpeed 40ms

Hide
Type "herdr --session agentprs-demo" Enter
Sleep 3s
Type "bash $AGENT_PRS_ROOT/demo/fake-agent.sh" Enter
Show

# The agent works and opens PRs in two repos.
Sleep 4.5s

# prefix+ctrl+p opens the strip for this agent and focuses it.
Ctrl+B
Ctrl+P
Sleep 3s
Screenshot assets/screenshot.png

# Move through the rows.
Type "j"
Sleep 800ms
Type "j"
Sleep 800ms

# m reveals merged and closed PRs.
Type "m"
Sleep 2.5s

# ? explains the keys.
Type "?"
Sleep 3.5s
Type "?"
Sleep 1s

# The same key closes the strip.
Ctrl+B
Ctrl+P
Sleep 1.5s
