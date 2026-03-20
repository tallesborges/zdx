# Changelog

## [0.4.0](https://github.com/tallesborges/zdx/compare/v0.3.0...v0.4.0) (2026-03-20)


### Features

* add active agent run tracking with monitor dashboard ([39640dc](https://github.com/tallesborges/zdx/commit/39640dc1c7b3be4e9f73f88b48cb10eca4d812af))
* **artifacts:** update skills, automations, and imagine command to use artifact_dir ([b7d543d](https://github.com/tallesborges/zdx/commit/b7d543dec4c4ee32ab74fa53a20e3154d01ba75f))
* **cli:** add 512px image size option for imagine command ([8da943d](https://github.com/tallesborges/zdx/commit/8da943d3a0e8d93090215ec2a7d94a982cea8296))
* **cli:** add exec event filtering and compatibility fixes ([7a280f2](https://github.com/tallesborges/zdx/commit/7a280f2332b15aecb89244c54bc0f741dcea6ab7))
* **cli:** add zdx imagine command for Gemini image generation ([#32](https://github.com/tallesborges/zdx/issues/32)) ([a4f4dbd](https://github.com/tallesborges/zdx/commit/a4f4dbddfc0bbc12d7e91ac8046de7cd0a77018e))
* **core:** add thread-scoped artifact directory resolution ([99789e3](https://github.com/tallesborges/zdx/commit/99789e30665b4a4423d814123a3674ad826bbce9))
* **core:** expose runtime context as ZDX_* environment variables ([f432317](https://github.com/tallesborges/zdx/commit/f43231784d2d2e7220912d597600be15cdef754e))
* **exec:** add --no-system-prompt support ([c908a2d](https://github.com/tallesborges/zdx/commit/c908a2d8724a3fb4bf081528bff851b373487618))
* **imagine:** add image editing support with --source flag ([c7f3071](https://github.com/tallesborges/zdx/commit/c7f3071abea77ee878798b74d5a5e60f44839451))
* **imagine:** default output to $ZDX_HOME/artifacts/ ([3fdbc90](https://github.com/tallesborges/zdx/commit/3fdbc90454ac4faea9c958a7a1074251e9d78ab1))
* **models:** detect reasoning from OpenRouter supported_parameters ([e282c07](https://github.com/tallesborges/zdx/commit/e282c0712ca939128ecdd8a5872f45a680751852))
* structured tracing + zdx-monitor TUI dashboard ([#34](https://github.com/tallesborges/zdx/issues/34)) ([f0061f1](https://github.com/tallesborges/zdx/commit/f0061f1350e40776ef10ffc22faaa945bb666d0d))
* **tui:** add /pwd, /open, /worktree-remove commands ([816d4b7](https://github.com/tallesborges/zdx/commit/816d4b754be1dc4df47b4bdd93309e4ac310a1e1))


### Bug Fixes

* **models:** remove deprecated gemini-3-pro-preview defaults ([2d51b8b](https://github.com/tallesborges/zdx/commit/2d51b8bcb7106332461e77f06e300325d37d23cd))
* **openai:** preserve responses phases and reasoning replay ([#38](https://github.com/tallesborges/zdx/issues/38)) ([5ab0b25](https://github.com/tallesborges/zdx/commit/5ab0b2584b82516acde9b714242616e1582416e3))
* **providers:** rename mimo provider to xiomi ([c6e7ef1](https://github.com/tallesborges/zdx/commit/c6e7ef17d63fce8b8d0e6324d27e9ed3d2daa645))
* **surfaces:** split output rules by interface ([158126b](https://github.com/tallesborges/zdx/commit/158126b4013dffe8382354823f06a79c49ad81f7))
* **turns:** unify terminal turn events ([64c9763](https://github.com/tallesborges/zdx/commit/64c97632458b9641bb826e34cc57d42a240ae5be))

## [0.3.0](https://github.com/tallesborges/zdx/compare/v0.2.0...v0.3.0) (2026-02-22)


### ⚠ BREAKING CHANGES

* **cli:** move daemon under automations and timestamp automation thread ids

### Features

* add MiniMax, Z.AI, and xAI providers ([6caabcd](https://github.com/tallesborges/zdx/commit/6caabcd0836cfae5995bd11254fd4e5379826132))
* **automations:** add global markdown automations with daemon ([#26](https://github.com/tallesborges/zdx/issues/26)) ([d32f230](https://github.com/tallesborges/zdx/commit/d32f2300716b8667292793421b9e1f7cf7ebd592))
* **bot:** stream agent events for live Telegram status updates ([#24](https://github.com/tallesborges/zdx/issues/24)) ([074d50c](https://github.com/tallesborges/zdx/commit/074d50c8a48387353b58f8d04bced0cd316d0f49))
* **bot:** switch Telegram formatting from Markdown to HTML ([0a04fc0](https://github.com/tallesborges/zdx/commit/0a04fc07d161fa93a4230d0b04d52478f2e655f2))
* **cli:** move daemon under automations and timestamp automation thread ids ([1424fe3](https://github.com/tallesborges/zdx/commit/1424fe30f3783cb88ff046b9883410c4ff385f85))
* **core:** persist partial content on interrupted responses ([#17](https://github.com/tallesborges/zdx/issues/17)) ([e58aa4f](https://github.com/tallesborges/zdx/commit/e58aa4fce3f449b47b0424c7979df9d41afe43b4))
* **debug-trace:** capture raw provider traffic ([d16d49f](https://github.com/tallesborges/zdx/commit/d16d49f1316e16718cee9502139a4e8724608ee9))
* **memory:** add proactive save suggestions for interactive surfaces ([d12bd8b](https://github.com/tallesborges/zdx/commit/d12bd8ba26ea061a69d56e4ae0d4c1bdecf08eb2))
* **providers:** register mimo provider ([30ea4e8](https://github.com/tallesborges/zdx/commit/30ea4e80e2bd6d8f8a48d86ec0d00ae2d1e771af))
* **threads:** add searchable thread CLI with automation-friendly output ([#27](https://github.com/tallesborges/zdx/issues/27)) ([13eca89](https://github.com/tallesborges/zdx/commit/13eca896173cd5a6bdb4eb065eed1fb9fe3b1e65))
* **tui:** add skill installer overlay ([aa3e81a](https://github.com/tallesborges/zdx/commit/aa3e81a555b25c64d6897cf8a1ceec6c23ae576a))
* **zdx-cli:** add telegram topic/send commands with parse modes ([4ac9be1](https://github.com/tallesborges/zdx/commit/4ac9be1f9df54a9dc22bef0bed7b8ea9cf6d1dd2))
* **zdx-cli:** support chat launch model and thinking overrides ([d45d5a8](https://github.com/tallesborges/zdx/commit/d45d5a85a91a5a0d4cd50703d2fedf7e4407a046))
* **zdx-core:** expose thread_search as a first-class agent tool ([#28](https://github.com/tallesborges/zdx/issues/28)) ([9a30805](https://github.com/tallesborges/zdx/commit/9a30805c4bfad6e302f072cedf26ec97b8c8f90e))


### Bug Fixes

* **cli:** correct cli_help tests for nested daemon subcommand ([329c8f9](https://github.com/tallesborges/zdx/commit/329c8f971b7b82df85b82e8f27675af3286d1eb2))
* **cli:** resolve lint issues and split command dispatch ([05de3a1](https://github.com/tallesborges/zdx/commit/05de3a1b4acbe7b58ebe7a2f59b1a4fe49a97462))
* fetch openai-codex models from models.dev API ([597c0c1](https://github.com/tallesborges/zdx/commit/597c0c12bc7a422493f87cf0febf28c01c0838d7))
* **models:** don't trust @ai-sdk/anthropic npm hint for non-claude models ([a3ef25f](https://github.com/tallesborges/zdx/commit/a3ef25fac74e4f93080ddc63f779cc6bd8807184))
* **models:** include apiyi and zen in models update ([8cd97b1](https://github.com/tallesborges/zdx/commit/8cd97b1d39b23dddebb71cdb501094d8391db069))
* **models:** use provider field for default model lookup ([9b12644](https://github.com/tallesborges/zdx/commit/9b1264422231a7530da9af452b4bdcac7b2b5490))
* omit max_tokens when unset and harden zen/apiyi routing ([43534fb](https://github.com/tallesborges/zdx/commit/43534fbf1f566c256c52021f3c276754a9635ffd))


### Miscellaneous Chores

* force version ([932f93c](https://github.com/tallesborges/zdx/commit/932f93c8ade4b8f984f175123df9a070796f84ac))

## [0.2.0](https://github.com/tallesborges/zdx/compare/v0.1.0...v0.2.0) (2026-01-31)


### Features

* add --no-tools exec flag ([2c6bfab](https://github.com/tallesborges/zdx/commit/2c6bfabd6600a026fc024305115949b4df2a7989))
* **bot:** improve Telegram response formatting guidelines ([#10](https://github.com/tallesborges/zdx/issues/10)) ([203d36d](https://github.com/tallesborges/zdx/commit/203d36d7cf5db852d33c03068ea3e80cae38de21))
* **cli:** add zdx bot subcommand ([d59cd4f](https://github.com/tallesborges/zdx/commit/d59cd4f7682b06596ac52732747d7382e0f63caf))
* **skills:** implement discovery and filters ([dc713f1](https://github.com/tallesborges/zdx/commit/dc713f1110f588a7cac9e81481223223bc524465))
* **tools:** add tool config and bot tool updates ([bdc1e35](https://github.com/tallesborges/zdx/commit/bdc1e35c18129daf69231bb5dbe56e9eb7dd9e2c))
* **worktree:** add worktree support and telegram opt-in ([468e67d](https://github.com/tallesborges/zdx/commit/468e67d389714c76d0ff0412e756b2dbe398e7c0))
