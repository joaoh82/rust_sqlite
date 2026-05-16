---
title: CRDTs for collaborative editing
tags: [crdt, distributed-systems]
---

# CRDTs for collaborative editing

Conflict-free replicated data types let two clients edit the same
document offline and merge the result deterministically. Two flavors
matter: state-based (CvRDTs) and operation-based (CmRDTs).

## When to reach for one

If your network is unreliable but you can ship every state mutation
through a message broker, CmRDTs win: smaller payloads, fewer wasted
bytes.

## What still bites you

Causality tracking — vector clocks specifically — grows with the
number of replicas. Yjs and Automerge invest a lot of effort in
compressing those metadata structures so they don't dominate the on-
wire payload for long-lived documents.
