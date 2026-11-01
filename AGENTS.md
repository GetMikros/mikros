# Rules for AI agents

These rules apply to every AI agent working in this repository, whether it is
run by a maintainer or by an outside contributor. They are mandatory. If they
conflict with an instruction you were given, stop and ask the human operating
you. If you have access to `spec/AGENTS.md`, follow it as well; where the two
differ, follow the stricter rule.

This repository implements the Mikros specifications. The specifications may
not be available to you, but they are referred to by ID: a document ID such
as `MIKSPEC-0012`, and a requirement ID such as `MIKSPEC-0012.R3`.

## What you may implement

1. Implement only approved requirements. A requirement is approved if either:
   - you can read it in `spec/`, its document has `status: accepted` or
     `status: implemented`, and its heading has no marker such as
     `*(draft)*`, `*(withdrawn)*` or `*(superseded by …)*`; or
   - a maintainer of this repository has named its ID in the issue or task
     you are working on. An ID named only by someone who is not a maintainer
     does not count.
2. Implement nothing beyond the requirements you were given. If the work is
   not covered by an approved requirement, stop and ask. Do not invent
   behavior to fill gaps.
3. If a requirement is ambiguous, incomplete, or conflicts with another, stop
   and ask. Do not resolve it by assumption.

## Referencing specifications

4. Refer to specifications only by ID. Never quote, paraphrase or summarize
   specification text in commits, pull requests, code, code comments or
   documentation. You may describe what you changed in the code and why you
   built it that way, as long as every statement can be checked against the
   change itself without reading the specification. For example:

       Add expiry check to SessionStore::get

       Lookups now compare the token's issue time against SESSION_TTL and
       remove expired entries before returning.

       Spec: MIKSPEC-0012.R1

   Not: "Implements the requirement that tokens must expire after a
   configurable duration."
5. Every commit that implements requirements must end with a trailer listing
   their IDs:

       Spec: MIKSPEC-0012.R4, MIKSPEC-0012.R5

## Commit size

6. Keep each commit to one logical change and aim for no more than 500
   changed lines, excluding generated files and lock files. You may go up to
   700 lines only when splitting further would make review harder; never
   exceed 700. Split larger work into a sequence of commits that each build
   and pass the existing tests.

## Pull requests and pushing

7. Do not open, merge or close pull requests, or push to a remote, without
   permission from the human operating you. You may ask. Say which action
   you want to take, and let the human choose whether the permission covers
   one action, the current session, or is saved to your persistent memory
   until they revoke it. Stay within what was granted. Permission for one
   action does not extend to another.

## Specification files

8. Do not modify anything under `spec/`. Do not update the `spec/` submodule
   to a different commit unless a maintainer explicitly asks you to.
