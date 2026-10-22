# Security policy

Musicable is pre-alpha and has no releases yet, so fixes go to `main` only.

## Reporting a vulnerability

Please report security problems privately, not in a public issue or pull
request: open the repository's **Security** tab and choose **Report a
vulnerability**
([direct link](https://github.com/maxbonifacio/Musicable/security/advisories/new)).
Say what happens, how to make it happen, and which commit you tried. This is
a hobby project run in spare time, so an answer can take a while, and there's
no bounty.

## What's in scope

Problems in Musicable's own code, for example:

- a project, audio, keymap or settings file that makes Musicable crash, hang
  or run code when it opens the file;
- memory unsafety in the `unsafe` code that talks to audio drivers and
  plugins;
- a CI workflow that exposes anything to a pull request from a fork.

Not in scope:

- Plugins. They run inside Musicable's process with its rights, as in other
  DAWs, so a plugin that misbehaves on purpose is a problem with that plugin.
  Load only plugins you trust. Plugin sandboxing is planned for later.
- Problems in a dependency. Please report them upstream, and tell us too if
  Musicable needs to change because of one.
