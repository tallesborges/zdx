# Changelog



## [0.2.0](https://github.com/tallesborges/zdx/compare/zdx-tui-v0.1.0...zdx-tui-v0.2.0) (2026-01-31)


### Features

* add --no-tools exec flag ([2c6bfab](https://github.com/tallesborges/zdx/commit/2c6bfabd6600a026fc024305115949b4df2a7989))
* **bot:** improve Telegram response formatting guidelines ([#10](https://github.com/tallesborges/zdx/issues/10)) ([203d36d](https://github.com/tallesborges/zdx/commit/203d36d7cf5db852d33c03068ea3e80cae38de21))
* **command-palette:** improve layout with 3-column design and description footer ([ed25fb3](https://github.com/tallesborges/zdx/commit/ed25fb360a9fe3a8c41d71389d00ef447b4ca77d))
* **input:** use $ prefix for bash commands ([05bffde](https://github.com/tallesborges/zdx/commit/05bffde6cac0f9a272d7afcd54495636ad3a49a1))
* **read-thread:** add tool and thread picker insert mode ([b336d73](https://github.com/tallesborges/zdx/commit/b336d7324f77a97b9046072a96f2aba116cd81d6))
* **skills:** implement discovery and filters ([dc713f1](https://github.com/tallesborges/zdx/commit/dc713f1110f588a7cac9e81481223223bc524465))
* surface loaded skills in tui ([c76078b](https://github.com/tallesborges/zdx/commit/c76078b5597914035fbe8f576d1cb387221cd4e7))
* **tools:** add tool config and bot tool updates ([bdc1e35](https://github.com/tallesborges/zdx/commit/bdc1e35c18129daf69231bb5dbe56e9eb7dd9e2c))
* **tui:** add internal input buffer ([6da2ad5](https://github.com/tallesborges/zdx/commit/6da2ad5aa477989bdd1d7c23e3195c72a13f4096))
* **tui:** add search filter to thread picker overlay ([b6dc9d4](https://github.com/tallesborges/zdx/commit/b6dc9d482e82e16b794fbf2096487c91d0cd06a2))
* **tui:** add turn timing cell with tool count ([20634d8](https://github.com/tallesborges/zdx/commit/20634d8c3f5e515728450f3e2e7f05914d0a2356))
* **tui:** add turn timing to status line ([5e546ea](https://github.com/tallesborges/zdx/commit/5e546ea9be5e8c5307ea6e2bf296f090404fc153))
* **tui:** add word and line cursor moves ([7345871](https://github.com/tallesborges/zdx/commit/734587153b01bf89f7a6d7624b4243e10f11e68b))
* **tui:** respect $EDITOR env var for /config and /models commands ([0e982e7](https://github.com/tallesborges/zdx/commit/0e982e7700c2942fa09543e5bd79761e1d4c7cad))
* **tui:** show read tool offset/limit ([7b37019](https://github.com/tallesborges/zdx/commit/7b370193aaa57f5bcf6ee74136739ca3caf12fbf))


### Bug Fixes

* **input:** treat punctuation as word boundaries for Option+Backspace ([24ac01a](https://github.com/tallesborges/zdx/commit/24ac01a78ac4ee9475870c1048884d8576afe668))
* **truncation:** harden read and streamline tests ([c299cec](https://github.com/tallesborges/zdx/commit/c299cecc9fe9548693e89af1d2ecb9f7d43912ab))
* **tui:** align tool bracket with truncation color ([0e6ce86](https://github.com/tallesborges/zdx/commit/0e6ce8664b0dd9ee3a9e5ec99dae0f732dfeb834))
* **tui:** box transcript mutation cell ([25cc71b](https://github.com/tallesborges/zdx/commit/25cc71b0d560f3c28d56f6f3b4ee97e397787841))
* **tui:** handle macOS word edits ([dcdfd5e](https://github.com/tallesborges/zdx/commit/dcdfd5ed6b618edde35722dcfe7bb825a9dbac8a))
* **tui:** keep forked user input out of history ([52eeac9](https://github.com/tallesborges/zdx/commit/52eeac99ee3619e0406eff2d1da88f3fc1533a99))
* **tui:** suppress read truncation warning with explicit limit ([e54787b](https://github.com/tallesborges/zdx/commit/e54787b6ed42b5c17d8b16b3d422541545e26d1e))
* **tui:** use linear scroll acceleration ([db8fe6c](https://github.com/tallesborges/zdx/commit/db8fe6ceca51f0466f4ef5a14dafe36516627466))


### Performance Improvements

* **tui:** skip cell line recalculation when width unchanged ([d586a16](https://github.com/tallesborges/zdx/commit/d586a160011110889b49b20d6c6d956b259be7d3))
