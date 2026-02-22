# Changelog

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
