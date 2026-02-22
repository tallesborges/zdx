# Changelog



## [0.2.0](https://github.com/tallesborges/zdx/compare/zdx-core-v0.1.0...zdx-core-v0.2.0) (2026-01-31)


### Features

* add gemini agentic prompt ([8b36147](https://github.com/tallesborges/zdx/commit/8b36147b7f9d0bc941cda26ec13da14a4dd04bc9))
* align codex headers with zdx ([b494920](https://github.com/tallesborges/zdx/commit/b494920a16cc90d26c77b43f52464b8d4480640a))
* **bot:** add Telegram forum topics support for supergroups ([3416ea4](https://github.com/tallesborges/zdx/commit/3416ea4d2d91200b35ae6215a6147f3ea66b5134))
* **bot:** improve Telegram response formatting guidelines ([#10](https://github.com/tallesborges/zdx/issues/10)) ([203d36d](https://github.com/tallesborges/zdx/commit/203d36d7cf5db852d33c03068ea3e80cae38de21))
* **core:** refine read_thread prompt ([5da0862](https://github.com/tallesborges/zdx/commit/5da08621fbd2558f84af467dfc92e129550afc86))
* **gemini:** add reasoning effort support with thought summaries ([f150562](https://github.com/tallesborges/zdx/commit/f150562606167f9f7976288e09c31ce8e4bbcc04))
* **input:** use $ prefix for bash commands ([05bffde](https://github.com/tallesborges/zdx/commit/05bffde6cac0f9a272d7afcd54495636ad3a49a1))
* **openai-codex:** add parallelism instructions to system prompt ([90be3e9](https://github.com/tallesborges/zdx/commit/90be3e9c59dc413d7cf1c8d2d3a2c7165eb5e433))
* **openai:** disable stream obfuscation ([1305368](https://github.com/tallesborges/zdx/commit/1305368531f09f0fde8e549743b4d08649d68a37))
* **providers:** add debug metrics wrapper for SSE stream instrumentation ([c370171](https://github.com/tallesborges/zdx/commit/c37017171ddcfdb0ba0c450814929b38472adc40))
* **providers:** share debug stream metrics ([df2b742](https://github.com/tallesborges/zdx/commit/df2b742e6d086b723a283fcb1e1250a90d7373f1))
* **read-thread:** add tool and thread picker insert mode ([b336d73](https://github.com/tallesborges/zdx/commit/b336d7324f77a97b9046072a96f2aba116cd81d6))
* simplify codex prompts and add text verbosity ([9c7373c](https://github.com/tallesborges/zdx/commit/9c7373c51c6ea02ef46c7f93557b8fb332557fde))
* **skills:** implement discovery and filters ([dc713f1](https://github.com/tallesborges/zdx/commit/dc713f1110f588a7cac9e81481223223bc524465))
* surface loaded skills in tui ([c76078b](https://github.com/tallesborges/zdx/commit/c76078b5597914035fbe8f576d1cb387221cd4e7))
* **tools:** add tool config and bot tool updates ([bdc1e35](https://github.com/tallesborges/zdx/commit/bdc1e35c18129daf69231bb5dbe56e9eb7dd9e2c))
* **worktree:** add worktree support and telegram opt-in ([468e67d](https://github.com/tallesborges/zdx/commit/468e67d389714c76d0ff0412e756b2dbe398e7c0))
* **zdx-bot:** add Telegram bot crate with DM-only agent bridge ([85fcfce](https://github.com/tallesborges/zdx/commit/85fcfce68d211d6e84b7847065d7267a3f5565a1))


### Bug Fixes

* **core:** rely on done encrypted_content ([f08a286](https://github.com/tallesborges/zdx/commit/f08a286d347da3970d23fb7ad933c0551de420d0))
* **truncation:** harden read and streamline tests ([c299cec](https://github.com/tallesborges/zdx/commit/c299cecc9fe9548693e89af1d2ecb9f7d43912ab))
