---
format: 1
name: release-notes
description: Release notes appended to a file
harness: copilot
model: gpt-5.4
tools: ["shell(git:*)", Read]
inputs:
  version:
    description: The version
  tone:
    required: false
  scope:
    default: all
    required: true
output:
  target: append
  path: notes/{version}.md
timeout: 2h
triggers: []
---
Write the notes of {version} ({tone}, {scope}) in {dir} about {selection}.
Answer as JSON: {{"version": "{version}"}}.
