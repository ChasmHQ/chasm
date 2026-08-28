"""The fullscreen frontend.

app.py      the Textual app: layout, key bindings, and the worker that drives the VM
pane.py     the scrolling, anchoring panel every pane is built on
panes.py    the panes themselves (source, stack, memory, storage, ...)
layout.py   cell/row/page construction shared by the panes
opcodes.py  opcode hints and stack-operand labels
theme.py    the colour palette, mirrored in sevm.tcss
"""
