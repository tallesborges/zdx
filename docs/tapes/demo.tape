Output "docs/assets/demo.gif"
Output "docs/assets/demo.mp4"

Set Shell bash
Set FontSize 20
Set Width 1600
Set Height 1000
Set Padding 24
Set Theme "Dracula"
Set WindowBar Colorful
Set Margin 60
Set MarginFill "#5B4B8A"
Set BorderRadius 12
Set TypingSpeed 45ms

Hide
Type "unset NO_COLOR; export PATH=$PWD/../.zdx/cargo-target/release:$PWD/../.zdx/cargo-target/debug:$PATH; clear"
Enter
Show

Type "zdx"
Sleep 500ms
Enter
Sleep 3s

Type "Read the README and summarize what zdx is in 3 short bullet points."
Sleep 800ms
Enter
Wait+Screen@90s /tool ·/
Sleep 2s
