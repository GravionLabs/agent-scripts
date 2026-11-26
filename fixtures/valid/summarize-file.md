---
name: summarize-file
description: Summary of a file in five bullet points
harness: claude
model: sonnet
tools: [Read]
inputs:
  audience:
    description: Who reads the summary
    default: developers
output:
  target: show
timeout: 5m
---

Summarize {file} for {audience} in five bullet points.
