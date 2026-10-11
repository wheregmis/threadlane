# Changelog

## [0.1.50](https://github.com/wheregmis/threadlane/compare/v0.1.49...v0.1.50) (2026-10-11)


### Bug Fixes

* **automation:** make New automation the primary action ([8a46161](https://github.com/wheregmis/threadlane/commit/8a46161b6e7d902e7b59a213344531684d17d7e0))
* **chat:** give the provider-less new-task screen one clear next step ([026d6da](https://github.com/wheregmis/threadlane/commit/026d6dab17481557a670ab89952146abad7f3cc6))
* **github:** keep issue and PR bodies below the detail title in hierarchy ([daafda9](https://github.com/wheregmis/threadlane/commit/daafda9f7aa5933b12fb716f7a23e31343a5966b))
* **github:** name a missing gh CLI and stop asking to select from a failed list ([05e43d1](https://github.com/wheregmis/threadlane/commit/05e43d1dcff01c0dd8942cada60cf7aecb029d9f))
* **right-panel:** show the panel chooser alone until a panel is open ([7d6fe9a](https://github.com/wheregmis/threadlane/commit/7d6fe9ab0414b90c1f4ab43e920e50d90210897e))
* **settings:** clear stale provider auth results on page entry ([41bda94](https://github.com/wheregmis/threadlane/commit/41bda940aa8cc92042cafed95b88f1d396fbf13a))
* **settings:** drop badges that repeat a switch and give pages distinct icons ([135842d](https://github.com/wheregmis/threadlane/commit/135842d273aa6d49e56c7ea033e58c5751b65d3c))
* **settings:** group Providers and name settings pages by their nav label ([037db22](https://github.com/wheregmis/threadlane/commit/037db220a5f7560b86eac11e51b7cc2b2f6b3483))
* **settings:** stop the custom ACP agent name reading as a filled value ([27b5966](https://github.com/wheregmis/threadlane/commit/27b59664dfe59db609fa4d29575bf4998110cdf2))
* **ui:** clarify right panel, new-task, GitHub detail and Providers screens ([7dbb796](https://github.com/wheregmis/threadlane/commit/7dbb796d162a2b58eb27941d8484a86e208836cc))
* **ui:** clear stale provider errors, name a missing gh, and focus Settings only on keyboard entry ([b5e04ef](https://github.com/wheregmis/threadlane/commit/b5e04efb4e35b688eacae8213ec8031fc207aac8))
* **ui:** keep sidebar empty-state copy on one line ([38c1b6f](https://github.com/wheregmis/threadlane/commit/38c1b6f7bf233c335ba172f79fb652fcfb5ee810))
* **ui:** remove redundant badges, distinguish icons, and promote primary actions ([f334575](https://github.com/wheregmis/threadlane/commit/f334575b670feb684c19b49a3b696c31e32623b0))
* **ui:** share one header row and status bar across workspace pages ([81f33db](https://github.com/wheregmis/threadlane/commit/81f33dbc6ff2f292ce91e45cc4329a0ee6ee7d26))
* **ui:** stop wrapped text overlapping in sidebar and settings ([3af1bd6](https://github.com/wheregmis/threadlane/commit/3af1bd6157dbdf3398c14af7fdfde351595ede5a))
* **workspace:** only move focus into Settings on keyboard entry ([ea423e3](https://github.com/wheregmis/threadlane/commit/ea423e3ed3c32f163077e0a5124f6e32e4b74843))

## [0.1.49](https://github.com/wheregmis/threadlane/compare/v0.1.48...v0.1.49) (2026-10-09)


### Features

* add ephemeral editor LSP backend ([d8fccb7](https://github.com/wheregmis/threadlane/commit/d8fccb738740301c44ace88af0e0017394f090a6))
* add find in review diffs ([c48b355](https://github.com/wheregmis/threadlane/commit/c48b35588b91bed81b9184b78cd87beb5d1d4d25))
* add live editor language services ([ce1f0bb](https://github.com/wheregmis/threadlane/commit/ce1f0bbc15a4ad9244138e7e429edf8299db4786))
* **catalog:** list anthropic/ models when ANTHROPIC_API_KEY is set ([8f2c171](https://github.com/wheregmis/threadlane/commit/8f2c171f9597469846c885dc1e6d394a1e4f7522))
* enhance editor completion, highlighting, and navigation ([6a7a221](https://github.com/wheregmis/threadlane/commit/6a7a221c7df25ca94ad166268c47525363dd995c))
* enhance editor completions and workbench controls ([c21ff38](https://github.com/wheregmis/threadlane/commit/c21ff382c8b11823e95f379e2af72a5fa4330285))
* find text in local Review diffs ([a604e80](https://github.com/wheregmis/threadlane/commit/a604e80368e74d794d342fc9b11994ae9cca7f61))
* integrate daemon-backed editor language services ([5832877](https://github.com/wheregmis/threadlane/commit/5832877a87ecc78dc2b31220ad5e7ab63e0a61b0))
* **provider:** add native Anthropic (Claude) provider ([f675ccf](https://github.com/wheregmis/threadlane/commit/f675ccf533c1eb486aec13ab73fdb7dba77e31c6))
* **provider:** add native Anthropic Messages provider ([0ae53a5](https://github.com/wheregmis/threadlane/commit/0ae53a5ceecdb01773ae5a53afe2c321d7304acb))


### Bug Fixes

* **acp:** launch npm-shim agents on Windows and send a plain cwd ([c6b8037](https://github.com/wheregmis/threadlane/commit/c6b80374b58e4ede04d809c02564f80091372671))
* **acp:** skip unsupported Windows PATHEXT candidates ([8fd22d2](https://github.com/wheregmis/threadlane/commit/8fd22d2f41b2fb753e4922461fc794cf81402e36))
* allow mobile pairing over Tailscale IPv4 ([49c83b0](https://github.com/wheregmis/threadlane/commit/49c83b0e8b6bd949c17d9ee71d92a9ed7e90a2ad))
* **chat:** deduplicate failures by exact retry payload ([b8a8b70](https://github.com/wheregmis/threadlane/commit/b8a8b7074024978789e3a7743adbca7bbf56a7fa))
* **chat:** resend exact failed prompts with image attachments ([6a60c7f](https://github.com/wheregmis/threadlane/commit/6a60c7ff665c826a11b8d959978c31a9a3c37de2))
* **chat:** restore rejected credential submissions to scoped drafts ([f4ac16f](https://github.com/wheregmis/threadlane/commit/f4ac16f1d9f3f552fa4f1211ffd8350dbef1ebe6))
* **chat:** retain retry payload for ACP preflight failures ([05b6403](https://github.com/wheregmis/threadlane/commit/05b64032b1b1659886826fd72b1435d64d580554))
* **discovery:** avoid caching stale session snapshots ([44e55b1](https://github.com/wheregmis/threadlane/commit/44e55b102e25244ce10cd6d89146c4c1e969a082))
* **editor:** guard saves against stale file versions ([89727a0](https://github.com/wheregmis/threadlane/commit/89727a074858ae23f50f37b43e02b8248e8b06a9))
* **editor:** preserve baselines across reload races ([42b7873](https://github.com/wheregmis/threadlane/commit/42b7873e7b6e7f5f5f80ab0b93c6d2feddf3ef19))
* **editor:** prevent saves from overwriting external changes ([9bf5076](https://github.com/wheregmis/threadlane/commit/9bf507615d69181e05553ed6d081ea0adcb90c92))
* **editor:** restore initial-load recovery ([29ca55c](https://github.com/wheregmis/threadlane/commit/29ca55c865aee761a762e92061739fb1ca8cad7c))
* harden editor LSP backend ([bab63f1](https://github.com/wheregmis/threadlane/commit/bab63f1e1770e93c715366060afbf823c6b94136))
* harden editor LSP concurrency and diagnostics ([56e4c6d](https://github.com/wheregmis/threadlane/commit/56e4c6de3f8ff827007f113a1b2528b02608b1d9))
* **hub:** durably settle children before allowing revival ([dc52c60](https://github.com/wheregmis/threadlane/commit/dc52c60b5bd391dac0dc61aadc40d55ee9c3f628))
* **hub:** durably settle subagents before allowing revival ([f3fb3c2](https://github.com/wheregmis/threadlane/commit/f3fb3c2b67498e3c7796e5dcae67c938bc9693ef))
* **hub:** serialize completion publication and settle retries ([c481a60](https://github.com/wheregmis/threadlane/commit/c481a60873ac824cd0b669939e4d38fe571e8774))
* keep offline completions visible during LSP polling ([e7964bf](https://github.com/wheregmis/threadlane/commit/e7964bffe225fdb7a481e463f731f90c2a604e0c))
* keep review whitespace label visible ([00cdcaf](https://github.com/wheregmis/threadlane/commit/00cdcaf8871ec657db32313f499a4c6df790115d))
* preserve review editor state and scrolling ([830fd4d](https://github.com/wheregmis/threadlane/commit/830fd4d994731eb1bd26481cca7455dc6908470e))
* preserve review find focus and visibility ([afe8207](https://github.com/wheregmis/threadlane/commit/afe820754ebad4de29e2a6fdddd1a7c10af81d72))
* prevent editor LSP presentation cancellation ([1c8b390](https://github.com/wheregmis/threadlane/commit/1c8b3901db8ef01911c2ce43a31b2aaf2cde034b))
* **provider:** keep Anthropic thinking data out of Gemini requests; map effort ([f977db3](https://github.com/wheregmis/threadlane/commit/f977db315da610f3d58a924b6f908ffe72eb9a71))
* **provider:** raise Anthropic default max_tokens and share env key helper ([b8cb6bd](https://github.com/wheregmis/threadlane/commit/b8cb6bd67c5fa39eb14652f71d16c17d06b43b99))
* **provider:** round-trip Anthropic thinking blocks through tool loops ([9e9b270](https://github.com/wheregmis/threadlane/commit/9e9b270845bbce6743a20d2ff09fc46472125120))
* **provider:** scope replayed Anthropic thinking blocks to their model ([74a966f](https://github.com/wheregmis/threadlane/commit/74a966f7b5ef07d88ad4c8d4102caefd727d0059))
* register editor workbench bindings during init ([45dd78c](https://github.com/wheregmis/threadlane/commit/45dd78ca7effd1cd80e6a5316cd4cf5918ef6bae))
* restrict editor LSP to global extension ([cdc11e5](https://github.com/wheregmis/threadlane/commit/cdc11e5afd5847dae8e61023347de2a995f5fce6))


### Performance Improvements

* **session:** avoid redundant journal scans and clones ([766c394](https://github.com/wheregmis/threadlane/commit/766c39466e6750a838960e83f96255c7b35e8634))

## [0.1.48](https://github.com/wheregmis/threadlane/compare/v0.1.47...v0.1.48) (2026-10-08)


### Features

* **agents:** require plan-first worker handoffs ([ed20d29](https://github.com/wheregmis/threadlane/commit/ed20d29c1969544a68e1fb87456a23d216db37ef))


### Bug Fixes

* **agents:** deduplicate Fusion handoff contract ([81a24c7](https://github.com/wheregmis/threadlane/commit/81a24c7bcf94cdf7b304a80e576027726b212958))
* coordinate worktree runtime ownership per session ([e4cb7a3](https://github.com/wheregmis/threadlane/commit/e4cb7a3a45f1e3d15305be473329ad4821de2133))
* discard prepared handoff when invalidating session runtime ([0b0e65d](https://github.com/wheregmis/threadlane/commit/0b0e65d026e74b8f89e2e810086accafeaa78e06))
* preserve shared owners across model changes and setup cancellation ([dfc92e9](https://github.com/wheregmis/threadlane/commit/dfc92e931e0355d1c67daabcb23462b42c3523bc))
* release cancelled preparation owners and bound construction gates ([0208606](https://github.com/wheregmis/threadlane/commit/020860638ad961eecfea96b68c8eb67bf284ec61))
* share session runtime ownership across desktop and mobile ([d8bf9cc](https://github.com/wheregmis/threadlane/commit/d8bf9ccb65a8de08e129d4c42ad621ad043701d2))


### CI

* add verified merge release note cleanup ([0a6a2d7](https://github.com/wheregmis/threadlane/commit/0a6a2d778b6a4d3bb95ca6e1eb32b536ae09b277))
* add verified merge release note cleanup ([a99c28d](https://github.com/wheregmis/threadlane/commit/a99c28d852d40d0141e02ec280dbd25e41ce27e5))
* prevent duplicate merge entries in release notes ([d3c54a7](https://github.com/wheregmis/threadlane/commit/d3c54a7bdbf50a3df3d413a7162a4e63e987b8f5))
* prevent verified merge duplicates in release notes ([2cc9a48](https://github.com/wheregmis/threadlane/commit/2cc9a484ea80228e127dbb5c5254543be0a1cf79))
* prevent verified merge duplicates in release notes ([6676ef8](https://github.com/wheregmis/threadlane/commit/6676ef834b590deafdd35af0183270eb6b08eaf3))
* prevent verified merge duplicates in release notes ([e7b3de9](https://github.com/wheregmis/threadlane/commit/e7b3de9dba1efc38352a05478d3ec95c76e42f58))

## [0.1.47](https://github.com/wheregmis/threadlane/compare/v0.1.46...v0.1.47) (2026-10-08)


### Bug Fixes

* **windows:** pin windows-core to 0.61 to match lb-wry; add Windows CI ([dfbe4af](https://github.com/wheregmis/threadlane/commit/dfbe4af3ab8b516b70544932a81ddbb52c7e00c5))
* **windows:** pin windows-core to 0.61 to match lb-wry; add Windows CI ([5fa0ad2](https://github.com/wheregmis/threadlane/commit/5fa0ad263959120ff9572b0163cf91a09113b2f5))

## [0.1.46](https://github.com/wheregmis/threadlane/compare/v0.1.45...v0.1.46) (2026-10-08)


### Features

* **editor:** preview Markdown from current document buffers ([2d08244](https://github.com/wheregmis/threadlane/commit/2d08244e44b27621f4562df0a435c31853614f51))
* **editor:** preview Markdown from retained document buffers ([5c4418f](https://github.com/wheregmis/threadlane/commit/5c4418f226ff36eed0764c2d94b0792c316b03b4))
* keep agentic Git workflows engaged through PR readiness ([4bca2b3](https://github.com/wheregmis/threadlane/commit/4bca2b3d0329145f5fb4a5a4dfb70ad2bc1b0a85))
* keep agentic Git workflows engaged through PR readiness ([1bd0391](https://github.com/wheregmis/threadlane/commit/1bd0391417654c9f619590612513ea0309a828e0))
* **mobile:** improve chat list and composer project selection ([5cd352c](https://github.com/wheregmis/threadlane/commit/5cd352c2d27905aab3683e9f18ae9554442f0c2c))
* **pairing:** persist paired desktop devices ([c2e2fc3](https://github.com/wheregmis/threadlane/commit/c2e2fc3a18d6eba7f9f3525cd695ad9f29daf98c))
* **pairing:** remember shared devices and reconnect across restarts ([ab937d3](https://github.com/wheregmis/threadlane/commit/ab937d300e7e4ce879069b14d1a430aa4d75c211))
* **settings:** add safe project removal without deleting files ([565815e](https://github.com/wheregmis/threadlane/commit/565815e9b21e6585617bb8e6a02806645ea4e271))


### Bug Fixes

* canonicalize local git branch refs ([ee27422](https://github.com/wheregmis/threadlane/commit/ee27422ac392162a4a1fe723fbd63aec033b40ae))
* **editor:** refresh Markdown preview only on content changes ([3340d07](https://github.com/wheregmis/threadlane/commit/3340d07a0ab43edf168b1912a57fdb5df8373fa2))
* **fusion:** persist lane contracts and monitor child outcomes ([4113391](https://github.com/wheregmis/threadlane/commit/41133915d442d7e40b799d1294cdd60d8d0e25d0))
* **fusion:** restore effective child lane contracts ([6937b3a](https://github.com/wheregmis/threadlane/commit/6937b3ab3867c923063451cf8858b4650eb6b153))
* **fusion:** restore interrupted children from saved contracts ([b500ab2](https://github.com/wheregmis/threadlane/commit/b500ab2d00b6e1d7769ca7db84184e7e1053f32a))
* **fusion:** stabilize delegation and preserve worker contracts ([37d3e02](https://github.com/wheregmis/threadlane/commit/37d3e021421e3ec0517eab0df956722e4ad5faa7))
* guard editor reopen and focus lifecycle ([3cdd1dc](https://github.com/wheregmis/threadlane/commit/3cdd1dc5ca834f39aa7af7f4c09d7057601a288e))
* guard PR readiness and filter agent review replies ([4b3e26c](https://github.com/wheregmis/threadlane/commit/4b3e26c3f46e2b8a522bdf361779a5eed38960a7))
* notify after rejected editor saves ([55f036c](https://github.com/wheregmis/threadlane/commit/55f036c9882bd111c447260c6dd1549c2c9c8467))
* **pairing:** address durable device correctness review ([a6e49b9](https://github.com/wheregmis/threadlane/commit/a6e49b9d4f97993f29d9fa1e91ba899281f1d45a))
* **pairing:** recover revoked ports and blocked clients ([386f0c6](https://github.com/wheregmis/threadlane/commit/386f0c6d1e8a7f4960e6bf43715c1d33f3fff208))
* preserve local branch metadata ([c67efcd](https://github.com/wheregmis/threadlane/commit/c67efcdbd0dd2e275d3d3f88eb45f71d764363f1))
* **projects:** guard live work and preserve exact removal paths ([433abc5](https://github.com/wheregmis/threadlane/commit/433abc5821cd0431d6278a1b2657caf900747597))
* reopen recently closed Editor files ([4527523](https://github.com/wheregmis/threadlane/commit/4527523314e4da0150d4fcd25d8ad46fab161e18))


### Performance Improvements

* optimize workspace workflows ([e7de212](https://github.com/wheregmis/threadlane/commit/e7de212994e9e3c61a24865ffeadb7e11bb39278))
* reduce redundant work across workspace workflows ([7eb7f2e](https://github.com/wheregmis/threadlane/commit/7eb7f2eab22cebbb94d44fc9c32c2c807684b4b2))


### Build System

* **deps:** bump robius-open from `71966dc` to `aea7a4f` ([e1f5c80](https://github.com/wheregmis/threadlane/commit/e1f5c80f4052cc6be186c28a4731d1d403120662))
* **deps:** bump robius-open from `71966dc` to `aea7a4f` ([8e322d3](https://github.com/wheregmis/threadlane/commit/8e322d380f714ee88599a482bb844e7129c5d897))
* **deps:** bump windows-core from 0.61.2 to 0.62.2 ([d84ec7b](https://github.com/wheregmis/threadlane/commit/d84ec7b1a899329bbfdc63f9dce35a4449b382a8))
* **deps:** bump windows-core from 0.61.2 to 0.62.2 ([bc339e5](https://github.com/wheregmis/threadlane/commit/bc339e5c30e486c133e71d4ad3dc339642fee90d))

## [0.1.45](https://github.com/wheregmis/threadlane/compare/v0.1.44...v0.1.45) (2026-10-08)


### Bug Fixes

* **deps:** pin webview2-com to 0.38.2 to match lb-wry on Windows ([49879d1](https://github.com/wheregmis/threadlane/commit/49879d18b0217a3db138ad94397f38e7796edee4))
* **deps:** pin webview2-com to 0.38.2 to match lb-wry on Windows ([8fdb01d](https://github.com/wheregmis/threadlane/commit/8fdb01d30c4a12a4b1e28783d06748592e8e3cdd))

## [0.1.44](https://github.com/wheregmis/threadlane/compare/v0.1.43...v0.1.44) (2026-10-07)


### Features

* add file editor selection to chat draft ([add8f01](https://github.com/wheregmis/threadlane/commit/add8f012d9a93f06393494d26fcc76ba9950158b))
* add file editor selection to chat draft ([e226a32](https://github.com/wheregmis/threadlane/commit/e226a32822392f1dcb774d875e84a5a39d3a43b8))
* **review:** navigate adjacent local Review diffs with Previous/Next file ([7c28167](https://github.com/wheregmis/threadlane/commit/7c281670ec7e14f68f0282013d3b43b426c3fac2))
* **review:** navigate adjacent local Review diffs with Previous/Next file ([6157a47](https://github.com/wheregmis/threadlane/commit/6157a47f88cc989cf35aabf46f733f41051f5fc2)), closes [#402](https://github.com/wheregmis/threadlane/issues/402)
* switch sessions with a window-local recent-visits picker ([fea00cf](https://github.com/wheregmis/threadlane/commit/fea00cfc3ae206c176c1ba4eacfdf0a3ee444c38))
* **ui-chat:** quote selected assistant text into the composer draft ([6d319a7](https://github.com/wheregmis/threadlane/commit/6d319a7b8292a684d9bd15672fb9ff532196a475))
* **ui-chat:** quote selected assistant text into the composer draft ([3248f6c](https://github.com/wheregmis/threadlane/commit/3248f6c050b4084bac2dcdbc1f4fb10153c1368d)), closes [#405](https://github.com/wheregmis/threadlane/issues/405)


### Bug Fixes

* compile the kit preview's draft strip ([d996bd8](https://github.com/wheregmis/threadlane/commit/d996bd873125ef20c77e14b07978aebe3d0b2032))
* harden the panel file open path ([6b86e23](https://github.com/wheregmis/threadlane/commit/6b86e23995a3748e86c76aeb93b92f10182b605b))
* **preview:** retain boundary focus for review diff navigation ([10d904b](https://github.com/wheregmis/threadlane/commit/10d904b810eb106c32020085ebff707eb0b46374))
* refresh add-selection controls and wire the panel file host ([ed4aa4a](https://github.com/wheregmis/threadlane/commit/ed4aa4af3b995f46b97d1223c8872a0d64c97fee))
* refresh remote inventory and allow navigation to pending sessions ([ae33f59](https://github.com/wheregmis/threadlane/commit/ae33f590dbf6143d97a3a6252b7b8a54e1418fc3))
* **review:** preserve diff scroll on refresh, isolate test fixture, drop nav tab stop ([c8f768e](https://github.com/wheregmis/threadlane/commit/c8f768e377c94896aa88454471ac53c06728795c))
* **ui-chat:** apply the menu-time snapshot when Quote selection is clicked ([d5e8f55](https://github.com/wheregmis/threadlane/commit/d5e8f55714e0b5e0e9a41dd11b5d48c5b2337833))
* **ui-chat:** ignore outer blank lines when quoting a selection ([f4948f7](https://github.com/wheregmis/threadlane/commit/f4948f7a0804b13a2f0b9b6f9ea1e8b79a3b1bea))
* **ui-chat:** quote selection menu click and trailing-newline handling ([398c7c8](https://github.com/wheregmis/threadlane/commit/398c7c856c844239824564dee72719779512221d))
* **ui-chat:** vendor quote icon and expire armed quotes on deselection ([c31a0f3](https://github.com/wheregmis/threadlane/commit/c31a0f3ac66b50378e820abb2faf924205b23f93))


### Build System

* **deps:** bump actions/configure-pages from 5 to 6 ([47a171e](https://github.com/wheregmis/threadlane/commit/47a171e0f3cfbecfef0d95d622cddfdc12d97a72))
* **deps:** bump actions/configure-pages from 5 to 6 ([ccd10e6](https://github.com/wheregmis/threadlane/commit/ccd10e6bc0c7a75a335980bca24638d452da9ef0))
* **deps:** bump actions/deploy-pages from 4 to 5 ([9487d71](https://github.com/wheregmis/threadlane/commit/9487d71e4758d92765f8f2862a0ef99650a29543))
* **deps:** bump actions/deploy-pages from 4 to 5 ([b525ae0](https://github.com/wheregmis/threadlane/commit/b525ae0d1c1871a8d93c109a5377614e3b97d174))
* **deps:** bump actions/upload-pages-artifact from 4 to 5 ([b2ae4fe](https://github.com/wheregmis/threadlane/commit/b2ae4feb2361244627bf70a38646844c126df946))
* **deps:** bump actions/upload-pages-artifact from 4 to 5 ([c5aeeb3](https://github.com/wheregmis/threadlane/commit/c5aeeb3fe165ef9ff5934d518e28e4a5731ea6b9))
* **deps:** bump hotpath from 0.26.1 to 0.28.4 ([a030179](https://github.com/wheregmis/threadlane/commit/a0301790e702c6d95cc131c5f76ca22a37be44ea))
* **deps:** bump hotpath from 0.26.1 to 0.28.4 ([1ea29c6](https://github.com/wheregmis/threadlane/commit/1ea29c6366139bac61f6f1fe55a9a40663cd434a))
* **deps:** bump rustix from 1.1.4 to 1.1.5 ([ab06c9a](https://github.com/wheregmis/threadlane/commit/ab06c9a373c777c8bc646ff3f89ef7c9fe42a82e))
* **deps:** bump rustix from 1.1.4 to 1.1.5 ([05aa364](https://github.com/wheregmis/threadlane/commit/05aa364d18893f6c1ae314afa1b264a2889e823d))
* **deps:** bump tokio from 1.53.1 to 1.53.2 ([55d1d6e](https://github.com/wheregmis/threadlane/commit/55d1d6eb8eaccde4f6a885b98de871bbf6247fba))
* **deps:** bump tokio from 1.53.1 to 1.53.2 ([b4682b3](https://github.com/wheregmis/threadlane/commit/b4682b36768c58cb1b8d6211c21d2f90efd95258))
* **deps:** bump webview2-com from 0.38.2 to 0.39.1 ([7feec46](https://github.com/wheregmis/threadlane/commit/7feec4649dca335dfbc5e48217d736db0900b226))
* **deps:** bump webview2-com from 0.38.2 to 0.39.1 ([2111dac](https://github.com/wheregmis/threadlane/commit/2111dac7dbac15784c92e9d7aae3c601b69d16bb))

## [0.1.43](https://github.com/wheregmis/threadlane/compare/v0.1.42...v0.1.43) (2026-10-07)


### Features

* **catalog:** add manual refresh for provider model caches ([26b00b9](https://github.com/wheregmis/threadlane/commit/26b00b968fa9ae3fb464d621c62930b53455cd9a))
* **catalog:** add manual refresh for provider model caches ([1a60ee8](https://github.com/wheregmis/threadlane/commit/1a60ee8e79a5e152abc2f244d58f12cd1d404d7e))


### Bug Fixes

* **automation:** distinguish blocked tasks from successful agent turns ([0d34f2a](https://github.com/wheregmis/threadlane/commit/0d34f2a36e4edc80a133456d8532bac4a189a0b2))
* **automation:** require explicit task outcomes for new runs ([7c6b438](https://github.com/wheregmis/threadlane/commit/7c6b43814e11884d32824ac3d2c888f482b0a8d5))
* **catalog:** invalidate provider-level model caches and guard status write ([44a1137](https://github.com/wheregmis/threadlane/commit/44a1137a6fd5aeb985f69964469e9b053499871a))
* **policy:** keep unconfirmed replacements and storage validation fenced ([d1d2c0e](https://github.com/wheregmis/threadlane/commit/d1d2c0e21161014417836d6047b934dbc02128d6))
* **policy:** restore tools.policy without taking the state-owner lease ([5659380](https://github.com/wheregmis/threadlane/commit/5659380e0550f80e9631d3b9bd496778d0264702))
* **policy:** restore tools.policy without taking the state-owner lease ([8c97323](https://github.com/wheregmis/threadlane/commit/8c97323ff18fa04420302569b236214b00f89e70))
* **preview:** handle RefreshModels in settings preview match ([944d71b](https://github.com/wheregmis/threadlane/commit/944d71b410942e7ae316948739b253d74e1d2ead))

## [0.1.42](https://github.com/wheregmis/threadlane/compare/v0.1.41...v0.1.42) (2026-10-06)


### Features

* add Threadlane website and live preview on GitHub Pages ([87b8bd3](https://github.com/wheregmis/threadlane/commit/87b8bd3a777e65474173bd2c54dd7b3103c36114))


### Bug Fixes

* use preview toolchain throughout Pages build ([8bae23c](https://github.com/wheregmis/threadlane/commit/8bae23cc8f0cceb74062a18bf995fde0d0018b0c))

## [0.1.41](https://github.com/wheregmis/threadlane/compare/v0.1.40...v0.1.41) (2026-10-05)


### Bug Fixes

* reconcile automation questions and remote projections ([0664d1a](https://github.com/wheregmis/threadlane/commit/0664d1ac6309cfd0447559d15f72f625d991a5b9))

## [0.1.40](https://github.com/wheregmis/threadlane/compare/v0.1.39...v0.1.40) (2026-10-03)


### Features

* embedded browser on Linux/X11 via WebKitGTK ([1a25156](https://github.com/wheregmis/threadlane/commit/1a25156ae2617c43da5119c4683674822b04a62e))
* support Linux as a desktop platform ([57cc3ee](https://github.com/wheregmis/threadlane/commit/57cc3ee6d5c62ab53bb198ef2bad097b118bef5d))
* support Linux as a desktop platform ([fd47e7d](https://github.com/wheregmis/threadlane/commit/fd47e7dfb8a9d8da2e8776a08c22720c9d66d0af))
* Windows platform support (browser, terminal, tools) ([d413ab9](https://github.com/wheregmis/threadlane/commit/d413ab9dd57a06ab98cdcbd445f33e561e2441f0))
* Windows support for app, terminal, and embedded browser ([3919c2b](https://github.com/wheregmis/threadlane/commit/3919c2b26c18d5a7049548d7a7e92f2eb3d57676))


### Bug Fixes

* select X11 backend via gdk::set_allowed_backends ([3675994](https://github.com/wheregmis/threadlane/commit/367599416bab1c4f64592dfe5e233fc2105f303b))


### CI

* address review — arm64 zip check, installable Linux desktop entry ([f15091c](https://github.com/wheregmis/threadlane/commit/f15091c9865fa8b59a4b75cc63176dbfb31b4953))
* build Linux and Windows packages in the release workflow ([a4949a3](https://github.com/wheregmis/threadlane/commit/a4949a313b0d42e2732d90efbe0e4154d5d034e1))
* release packages for Linux and Windows ([cc983da](https://github.com/wheregmis/threadlane/commit/cc983dac291efa0d075d902df3c3a98335da9e77))


### Maintenance

* **main:** release 0.1.39 ([08c1ff8](https://github.com/wheregmis/threadlane/commit/08c1ff8e394ee4512ea3c47a0f8a26538b51117d))
* **main:** release 0.1.39 ([16ee79c](https://github.com/wheregmis/threadlane/commit/16ee79c733bc3df6bbd6a661cfda3d4a5ca65027))

## [0.1.39](https://github.com/wheregmis/threadlane/compare/v0.1.38...v0.1.39) (2026-10-03)


### Features

* add checkout-scoped Find in files navigation ([63ff254](https://github.com/wheregmis/threadlane/commit/63ff2546a4d7737b7741efd72d2c006de7bd6077))
* search project conversations by message text ([7a7bb3b](https://github.com/wheregmis/threadlane/commit/7a7bb3b27ecb651a9e0137e912801a36d5d6fec3))
* search project conversations by message text ([fcaf73a](https://github.com/wheregmis/threadlane/commit/fcaf73a7e00165f2b2cdbbcfa54911203543812b)), closes [#378](https://github.com/wheregmis/threadlane/issues/378)


### Bug Fixes

* land find handoff from any tab and restore prepend splice count ([9af918c](https://github.com/wheregmis/threadlane/commit/9af918cfdef71a4aa96476c5ade700308fc22121))
* reject search root symlinks and reload clean editor tabs ([5d49ee8](https://github.com/wheregmis/threadlane/commit/5d49ee8d956861408d9b0dd9d37ec5210ceb7641))


### Performance Improvements

* reuse validated file descriptors during content search ([a2798e9](https://github.com/wheregmis/threadlane/commit/a2798e995ef050f8e357c84e49cf7ac20a0e2d0d))

## [0.1.38](https://github.com/wheregmis/threadlane/compare/v0.1.37...v0.1.38) (2026-10-01)


### Features

* **sidebar:** snooze settled sessions with a local deadline ([3487af5](https://github.com/wheregmis/threadlane/commit/3487af5ea8330bac856225525df25336993f8323))
* **sidebar:** snooze settled sessions with a local deadline ([6146434](https://github.com/wheregmis/threadlane/commit/6146434865d001c20cce22c3487adf91d0952cce))


### Bug Fixes

* address review findings on daemon project-io ([2fcfe9e](https://github.com/wheregmis/threadlane/commit/2fcfe9efd1c235c26e35b25cef1d7f2aff048385))
* drain snooze acks on the pump, bound deadlines, retry failed deletions ([83196de](https://github.com/wheregmis/threadlane/commit/83196de7366780bb2a46193685ae65bc80868814))


### Code Refactoring

* **ui:** share bubble styling across clients ([8c9c251](https://github.com/wheregmis/threadlane/commit/8c9c251ac0b2b98573560de9effee11a5c470658))
* **ui:** share bubble styling across clients ([af6bfcb](https://github.com/wheregmis/threadlane/commit/af6bfcbe3655c68b515fd1693eaf5ac1f3139932))

## [0.1.37](https://github.com/wheregmis/threadlane/compare/v0.1.36...v0.1.37) (2026-10-01)


### Bug Fixes

* **harness:** reproject main leaf on first OperationStarted ([a91eb34](https://github.com/wheregmis/threadlane/commit/a91eb344ea6180818be9f6601ea8597cd89aa7d4))
* Improve right panel surface layout ([fab5342](https://github.com/wheregmis/threadlane/commit/fab53427bd827e5c3ffdce3a7461e3a0227d0302))
* **ui-chat:** lay out prompt rail beside transcript viewport ([7d26cb3](https://github.com/wheregmis/threadlane/commit/7d26cb335b2294824b82e1ccde82779e0e47cb80))


### Performance Improvements

* **harness:** eliminate per-commit reparses and per-attempt rescanning ([b62bcfe](https://github.com/wheregmis/threadlane/commit/b62bcfe28cf6c914f886f08771757f08afb6f769))


### Code Refactoring

* **ui:** Simplify Git commit controls ([7c407e1](https://github.com/wheregmis/threadlane/commit/7c407e1612c69fc333b6cf0baaebc7195c08ddaa))


### Maintenance

* address reviews ([805f024](https://github.com/wheregmis/threadlane/commit/805f02462bd1f534c52b56d41491cb59c529321f))

## [0.1.36](https://github.com/wheregmis/threadlane/compare/v0.1.35...v0.1.36) (2026-10-01)


### Features

* **terminal:** open visible web links through explicit browser actions ([9b9e102](https://github.com/wheregmis/threadlane/commit/9b9e102bf66f9de043fb6af44c23c3feec4329c4))
* **terminal:** safely open visible URLs in Browser tabs ([c2cdf1f](https://github.com/wheregmis/threadlane/commit/c2cdf1f2d65e2d0c5cb00de058ecd6ef89ef2708))


### Bug Fixes

* **a11y:** expose terminal link picker scope in accessible name ([3ce6c68](https://github.com/wheregmis/threadlane/commit/3ce6c68b2f33d529469f64cbe40460328bf623ab))
* **browser:** compose webviews beneath GPUI overlays ([6b86016](https://github.com/wheregmis/threadlane/commit/6b860168813fb060045de6279f9f70efb14d6d4f))
* **browser:** compose webviews beneath GPUI overlays ([1af5ec7](https://github.com/wheregmis/threadlane/commit/1af5ec77090df0a119d7f28b3d4825a5125e798f))
* **mobile:** Adopt iOS scene lifecycle ([8188fd9](https://github.com/wheregmis/threadlane/commit/8188fd9650b9f148667e6dd4eb010243b769fc40))

## [0.1.35](https://github.com/wheregmis/threadlane/compare/v0.1.34...v0.1.35) (2026-09-30)


### Features

* add project findings and memory recall to manage_memory tool ([8226a2e](https://github.com/wheregmis/threadlane/commit/8226a2e670c90ef578b264fa21a2a9b95213741a))
* add project findings and memory recall to manage_memory tool ([83cd655](https://github.com/wheregmis/threadlane/commit/83cd655e6580de2d87a7677460a058e92c5f134a))
* **memory:** bound evidence scan budgets during recall ([acbec19](https://github.com/wheregmis/threadlane/commit/acbec19d797cc7a2fa6a0bdb96b71e37e91571a6))


### Bug Fixes

* address local Review lifecycle and diff feedback ([e7d0ad6](https://github.com/wheregmis/threadlane/commit/e7d0ad6bf808ede1aa635eef079b433360129d9d))

## [0.1.34](https://github.com/wheregmis/threadlane/compare/v0.1.33...v0.1.34) (2026-09-30)


### Features

* **right-panel:** simplify review rows and view diff label ([cf239cc](https://github.com/wheregmis/threadlane/commit/cf239cc3cc240531758afae6e352a22369207692))


### Bug Fixes

* **sidebar:** use foreground colors for the threadlane icon ([a515c60](https://github.com/wheregmis/threadlane/commit/a515c605f11dfeeb6c4051201cfbfeffbdd57526))


### Maintenance

* **deps:** bump gpui-component from `f8cd486` to `201b55a` ([7f016c6](https://github.com/wheregmis/threadlane/commit/7f016c6cd44697b15f6ed43b393d977601de9132))
* **deps:** bump gpui-component from `f8cd486` to `201b55a` ([78c17f6](https://github.com/wheregmis/threadlane/commit/78c17f6bf844958a4da434592775e1281812e1bb))
* **deps:** bump hotpath from 0.26.0 to 0.26.1 ([ebafc88](https://github.com/wheregmis/threadlane/commit/ebafc88f536f322e92d24997e70af9b06087fc1e))
* **deps:** bump hotpath from 0.26.0 to 0.26.1 ([07e8934](https://github.com/wheregmis/threadlane/commit/07e8934d67608cdee74a60bd2fe6a00410fbae6c))
* **deps:** bump thiserror from 2.0.20 to 2.0.21 ([bca44c4](https://github.com/wheregmis/threadlane/commit/bca44c42a6072612fde47f472bdd6f8469dadaee))
* **deps:** bump thiserror from 2.0.20 to 2.0.21 ([179014c](https://github.com/wheregmis/threadlane/commit/179014c745de229d45520c2f703ea74068e70ab2))
* **deps:** switch GPUI dependencies to pinned gpui-fast revision ([35b9970](https://github.com/wheregmis/threadlane/commit/35b9970ff08b67adbd4ba2adb3780853786dda88))

## [0.1.33](https://github.com/wheregmis/threadlane/compare/v0.1.32...v0.1.33) (2026-09-30)


### Features

* **git:** add branch context menu with safe deletion ([beb1e86](https://github.com/wheregmis/threadlane/commit/beb1e86d8466d447b1d2f2dd04f8899bb032fc1d))
* **git:** add safe branch deletion context menu ([4421157](https://github.com/wheregmis/threadlane/commit/4421157b3f7fe2b4077178f1bd9872d61c66f2a0))
* **models:** add GPT-6.1 Sol for OpenAI and Codex ([20b736e](https://github.com/wheregmis/threadlane/commit/20b736e48c8f4600c1b49149ddfe77569ae6fc26))
* **models:** register GPT-6.1 Sol for OpenAI and Codex ([7cc3a82](https://github.com/wheregmis/threadlane/commit/7cc3a8228210ec5c32b7792fa566125e0abd8b0c))
* **terminal:** find in terminal output ([e2134a8](https://github.com/wheregmis/threadlane/commit/e2134a8f20050f39c1d2a72b09f4bad3851bedc3))
* **terminal:** find in terminal output ([716433e](https://github.com/wheregmis/threadlane/commit/716433ea2b595f726e00e3058cb67567908c9108)), closes [#293](https://github.com/wheregmis/threadlane/issues/293)


### Bug Fixes

* **git:** disable local deletion for remote branch rows ([c0a1157](https://github.com/wheregmis/threadlane/commit/c0a11577af1a3d366f08888c20b4da40e4c30966))
* **openai:** reject unsupported GPT-6.1 Sol API-key tools ([d5ba294](https://github.com/wheregmis/threadlane/commit/d5ba294817679aab0e24d8708a766ace434dbcab))


### Maintenance

* better review ([6fa541f](https://github.com/wheregmis/threadlane/commit/6fa541fc7d923f81ea8a15255312d8b0f2bda7df))

## [0.1.32](https://github.com/wheregmis/threadlane/compare/v0.1.31...v0.1.32) (2026-09-30)


### Features

* **chat:** add safe preview for staged images ([5853d73](https://github.com/wheregmis/threadlane/commit/5853d7337da2b87c14dbdf90f9d70ca8131d2d5a))
* **chat:** add safe preview for staged images ([6f69e82](https://github.com/wheregmis/threadlane/commit/6f69e8298856f35268a0380e9156cba51bd581e3))
* move trajectory into the right panel ([6167fea](https://github.com/wheregmis/threadlane/commit/6167fea7c88274e148bd56ba572a11e43941d5ba))
* **sidebar:** move title regeneration to session context menus ([ca0d7bc](https://github.com/wheregmis/threadlane/commit/ca0d7bc6b1685563617649cb9684402d4bbe1866))
* **ui:** Refine task, issue, and workspace UI ([7e5fe9b](https://github.com/wheregmis/threadlane/commit/7e5fe9bee26b193873241a9a44491755b6297801))
* **ui:** Refine task, issue, and workspace UI ([e3fd1fc](https://github.com/wheregmis/threadlane/commit/e3fd1fc846f13711b61866f8920185c7dfac8ed2))


### Bug Fixes

* **chat:** compile image preview against image 0.25 reader API ([b1cc53e](https://github.com/wheregmis/threadlane/commit/b1cc53e109118777e917918ef733958364588d06))
* **chat:** prevent disclosure header content clipping ([3d1e430](https://github.com/wheregmis/threadlane/commit/3d1e4303614e52e92453ab42439153c11acdfc54))
* **chat:** prevent thought-process and tool header clipping ([1eb386f](https://github.com/wheregmis/threadlane/commit/1eb386f1452a8ba576af5d80669252154318accb))
* **runtime:** recover torn trailing journal lines on read ([5ee47f7](https://github.com/wheregmis/threadlane/commit/5ee47f79bd27b266a0fb13d4131d701a6a29e9ca))
* **tools,state,coding-agent,right-panel:** clean up worktree cargo target cache on teardown ([61407e9](https://github.com/wheregmis/threadlane/commit/61407e9135cdf2f328cf46c0d80ecaca761bcbba))


### Maintenance

* **cargo:** disable debug symbols in dev profile to reduce build size ([82ec55c](https://github.com/wheregmis/threadlane/commit/82ec55c9914061a5877cb71b80aa2426fcdf025c))
* only working ([44bd33a](https://github.com/wheregmis/threadlane/commit/44bd33a340aada4fabbb8c7dce85a80e532c8d97))
* only working ([cda8ba6](https://github.com/wheregmis/threadlane/commit/cda8ba684c87fe8108c4ff5b5b439bc7c763315a))

## [0.1.31](https://github.com/wheregmis/threadlane/compare/v0.1.30...v0.1.31) (2026-09-29)


### Features

* add session title and draft PR field regeneration ([5ec81c7](https://github.com/wheregmis/threadlane/commit/5ec81c7cf95bc1249368eb363981281d6a4cf87d))
* **right-panel:** add PR field generation prompts and diff helpers ([a87723a](https://github.com/wheregmis/threadlane/commit/a87723ad2b4272e20e69751feb29689fbe9e8087))
* **right-panel:** add PR field generation prompts and diff helpers ([7e48aed](https://github.com/wheregmis/threadlane/commit/7e48aedf2eee226f5ba791b4a1597a1a9b9ad907))


### Bug Fixes

* address PR generation review feedback ([fee9c45](https://github.com/wheregmis/threadlane/commit/fee9c45dc61460e261034d72c313dd315147f212))
* **workspace:** observe command palette query changes ([552f17d](https://github.com/wheregmis/threadlane/commit/552f17dbc7f0c3847f1e136b9ada0bbee5f11137))


### Maintenance

* some ui nits ([1f631f5](https://github.com/wheregmis/threadlane/commit/1f631f5de1120dcc7ea414c20ab31d1823585d33))

## [0.1.30](https://github.com/wheregmis/threadlane/compare/v0.1.29...v0.1.30) (2026-09-28)


### Features

* refine sidebar ([34aeb9d](https://github.com/wheregmis/threadlane/commit/34aeb9ddb444cbd229e73456921b5c54147f84a6))
* **sidebar:** add pins and quick filters ([bb55fd5](https://github.com/wheregmis/threadlane/commit/bb55fd59a8d7ff91314babf7dc88a676f24397ce))
* **sidebar:** add pins and quick filters ([f704732](https://github.com/wheregmis/threadlane/commit/f70473287497fc5137124b8cac0c9a7416600fd7))
* **sidebar:** add session status filters and activity badges ([df5314c](https://github.com/wheregmis/threadlane/commit/df5314cd034405d7f0254f76e2d75e12441e373e))
* **ui:** improve workspace layout and compact chat controls ([775f05e](https://github.com/wheregmis/threadlane/commit/775f05eac3823c4e02738f3b34fbe84f76f1c4a2))
* **ui:** refresh sidebar styling and branding ([6ce435d](https://github.com/wheregmis/threadlane/commit/6ce435d375d0d619217e097aba537b37444ba737))


### Bug Fixes

* defer refreshed close confirmation ([d70faa9](https://github.com/wheregmis/threadlane/commit/d70faa9d5b636c2fa4a75363aca219e5ca36f523))
* refresh active work close confirmation ([24eb8c1](https://github.com/wheregmis/threadlane/commit/24eb8c106453db410c078b75dc01f794bd5a9aca))
* **ui:** include scheduled turns in close warning ([35f71d1](https://github.com/wheregmis/threadlane/commit/35f71d1789ba05b069d9b10b4bf9d1ec675db0c5))


### Maintenance

* Remove obsolete Fusion enhancement notes ([5309273](https://github.com/wheregmis/threadlane/commit/53092732d8e1c043d9baf829da36c297ddb7b605))

## [0.1.29](https://github.com/wheregmis/threadlane/compare/v0.1.28...v0.1.29) (2026-09-27)


### Features

* **automation:** add run deletion and run modes ([3a178ad](https://github.com/wheregmis/threadlane/commit/3a178adad9ccf29c8aa55e7604875dd5e3d9c05a))
* **chat:** find and jump to matching conversation messages ([70431fb](https://github.com/wheregmis/threadlane/commit/70431fb77db71831425b325f9c350afb3b3bacc0))
* **chat:** find and navigate matching conversation messages ([5303128](https://github.com/wheregmis/threadlane/commit/530312868cce83866aabee0516bc36399b8357bd))
* enhance environment panel with Git workspace shortcuts ([fb2ad35](https://github.com/wheregmis/threadlane/commit/fb2ad353eb6ec389bd503c538371a78c66ba2c57))
* **settings:** add manual application update controls ([887a090](https://github.com/wheregmis/threadlane/commit/887a0901376c3b1e4cdc3556bf8e71d6838faf8f))


### Bug Fixes

* address environment panel accessibility and untracked totals ([b9b971e](https://github.com/wheregmis/threadlane/commit/b9b971e115a3065a3574f03a29e90cf9143917be))
* **chat:** constrain question card width ([3d69edf](https://github.com/wheregmis/threadlane/commit/3d69edf855c9a13a38eb74fc7efa66977b1f72e9))
* **chat:** constrain question card width ([830833f](https://github.com/wheregmis/threadlane/commit/830833f5b0e4b9a661350ef08d3bc91731653afc))
* **chat:** keep conversation find usable during streaming ([f9f1a9a](https://github.com/wheregmis/threadlane/commit/f9f1a9ae53f196390373a9451a564808149eea1a))
* **github:** Scope PR tasks to live branches ([7fb720d](https://github.com/wheregmis/threadlane/commit/7fb720d405e8794dd44c9ee8b29e50a32e5b532e))
* **updater:** compact update controls beside sidebar Settings ([9c4de5c](https://github.com/wheregmis/threadlane/commit/9c4de5c387f95df386d891faed1eb0aa44837159))
* **updater:** move update controls beside sidebar settings ([f7365e1](https://github.com/wheregmis/threadlane/commit/f7365e1590193f6a1fe16092e62c996b3c89bd5a))

## [0.1.28](https://github.com/wheregmis/threadlane/compare/v0.1.27...v0.1.28) (2026-09-26)


### Bug Fixes

* **tools,runtime,browser:** steer model away from verbatim-retry waste loops ([7a96180](https://github.com/wheregmis/threadlane/commit/7a961808dd01e26a543f23c09123d09907e6e56b))

## [0.1.27](https://github.com/wheregmis/threadlane/compare/v0.1.26...v0.1.27) (2026-09-26)


### Features

* Add persistent process diagnostics ([771fa56](https://github.com/wheregmis/threadlane/commit/771fa568260ca71ccb964a7c80028e8e9dab30d0))


### Bug Fixes

* expose create_automation in the filtered tool schema ([4e67e52](https://github.com/wheregmis/threadlane/commit/4e67e52879f3e485a8c4baf1be7a0f2764bd5334))
* **runtime:** expose all registered tools to the model by default ([25c1668](https://github.com/wheregmis/threadlane/commit/25c1668e67babd6ffb00093cab7f15d010efc871))
* **ui:** Preserve agent activity panel space ([942f162](https://github.com/wheregmis/threadlane/commit/942f162c049a3673bc035e2fcba9428634b28ae8))

## [0.1.26](https://github.com/wheregmis/threadlane/compare/v0.1.25...v0.1.26) (2026-09-26)


### Features

* **automations:** add durable sidebar automations ([e108f3e](https://github.com/wheregmis/threadlane/commit/e108f3e6dbf8f225fa01ddbc8815edf66466addb))
* **automations:** add durable sidebar automations ([801adda](https://github.com/wheregmis/threadlane/commit/801addaa5106bb3d117bfbde3d9e6dc63cba7ebc))
* **automations:** create scheduled tasks from chat ([1dae746](https://github.com/wheregmis/threadlane/commit/1dae7465b6ed7231e8ed00616ca7c082eb34bcee))
* **browser:** expose existing browser tabs to agents ([86114d4](https://github.com/wheregmis/threadlane/commit/86114d4a3f4aaaa27788a638618079bc95ab2193))
* **browser:** let agents manage multiple browser tabs ([de0b316](https://github.com/wheregmis/threadlane/commit/de0b316a69e8b8b8c968bda62f5b6bdbd8689f1c))
* **chat:** show queued messages above the composer ([f2705e2](https://github.com/wheregmis/threadlane/commit/f2705e2df81de07f69b6bd887cb6ac81fb905af1))
* **chat:** show queued messages above the composer ([4426780](https://github.com/wheregmis/threadlane/commit/44267803fdb3d4cc935389188ac21e2d9efe384b))


### Bug Fixes

* **automations:** bound reviewed history and keep dispatch resilient ([cd6a8ba](https://github.com/wheregmis/threadlane/commit/cd6a8bacafe23e364833927e141454daca011cf9))
* **automations:** preserve schedules and project-scoped defaults ([5435128](https://github.com/wheregmis/threadlane/commit/5435128f1909725dbdab653037ed9975d4e5c43d))
* **browser:** preserve unsaved editor work during agent tab operations ([b800329](https://github.com/wheregmis/threadlane/commit/b800329c013240fb8f4834a461960d99c139a90f))
* **chat:** reset transcript list when queue filtering changes rows ([cbd2935](https://github.com/wheregmis/threadlane/commit/cbd29350dd1b64bcd0b391f87f9b9fde39106c09))

## [0.1.25](https://github.com/wheregmis/threadlane/compare/v0.1.24...v0.1.25) (2026-09-26)


### Features

* **agents:** add resizable split between overview and detail ([dd5e0f1](https://github.com/wheregmis/threadlane/commit/dd5e0f1f46f26376dc6f78c2e02e0b640db695a0))
* **agents:** show agent status and task in panel ([23eb745](https://github.com/wheregmis/threadlane/commit/23eb745d893693ec3d8664e7f8632cbda0d3369a))
* **terminal:** add display options for font, spacing, and background ([f8cd307](https://github.com/wheregmis/threadlane/commit/f8cd307e9725d90df0c7ee34b49c87df43d2a47a))
* **ui:** add GitHub sidebar navigation ([9df5424](https://github.com/wheregmis/threadlane/commit/9df5424b3bca54a52985195cdf01eab444b693be))
* **ui:** Redesign agents panel with profile tabs ([e025219](https://github.com/wheregmis/threadlane/commit/e0252196f048cc5e8ae8ea90799e3e5a058a9751))


### Bug Fixes

* **agents:** select available agent and clarify panel labels ([c18780c](https://github.com/wheregmis/threadlane/commit/c18780ca07fdb39ec4b9b1a26b48fd26c493b538))
* **github:** Keep PR code view responsive ([54aa3a9](https://github.com/wheregmis/threadlane/commit/54aa3a971e0e6a650ded366dbea57ee8c753a5ff))
* **ui:** improve agents panel profile layout ([ac62e47](https://github.com/wheregmis/threadlane/commit/ac62e47a5404860076b3d08f496ef0375aaab73c))

## [0.1.24](https://github.com/wheregmis/threadlane/compare/v0.1.23...v0.1.24) (2026-09-26)


### Features

* **chat:** expand subagent activity UX ([ab524ee](https://github.com/wheregmis/threadlane/commit/ab524ee3429ee225551b45f2810c5d61f8c74dda))
* **chat:** improve subagent activity popover ([7456b39](https://github.com/wheregmis/threadlane/commit/7456b391f1779c3debb1ea7c1618906fc383e3da))
* **ui:** move agent activity into right panel ([597589c](https://github.com/wheregmis/threadlane/commit/597589cd11b09cb7f384d6938b4546dff63b9b50))


### Bug Fixes

* expose hub in filtered tool schemas ([fb621bf](https://github.com/wheregmis/threadlane/commit/fb621bf3cf71b4d228a2becd8bdc149d1e3efd61))
* expose hub in filtered tool schemas ([1e6cde4](https://github.com/wheregmis/threadlane/commit/1e6cde47cf5eb122588fe52314a6f4305974b8f1))


### Performance Improvements

* **github:** coalesce list requests across repository checkouts ([dec0c48](https://github.com/wheregmis/threadlane/commit/dec0c48e36694f2f059e74b18927e5f24d3b47a8))

## [0.1.23](https://github.com/wheregmis/threadlane/compare/v0.1.22...v0.1.23) (2026-09-26)


### Features

* **github:** offer Fusion mode in issue Start Task dialog ([9f452e7](https://github.com/wheregmis/threadlane/commit/9f452e79462a9f8a8a35aa28e915e2794cbb439d))
* **github:** offer Fusion mode in issue Start Task dialog ([5683d5f](https://github.com/wheregmis/threadlane/commit/5683d5fcfa644c169bb729881f995b3a47a8e8cd)), closes [#243](https://github.com/wheregmis/threadlane/issues/243)
* Support ACP commit messages ([e8eeab7](https://github.com/wheregmis/threadlane/commit/e8eeab7365ee4df852717e35b3628d1831cac24b))
* **worktree:** add recovery for unavailable session worktrees ([08d3508](https://github.com/wheregmis/threadlane/commit/08d3508839f4864ad6cf97e4aa33da4fbed4c688))


### Bug Fixes

* **openai:** Prefer owned logins in model refresh ([6c53810](https://github.com/wheregmis/threadlane/commit/6c5381074272543f862bfe7389d8b79c843ae047))
* Preserve sibling directories during tree scans ([ec0eb63](https://github.com/wheregmis/threadlane/commit/ec0eb639c781591c09d3c7b40f024b79598a190a))
* remove duplicated toast ([b73d50a](https://github.com/wheregmis/threadlane/commit/b73d50ad188ed91407a874892a1e26dcb72204b4))
* **workspace:** address PR review comments on worktree recovery ([185fd56](https://github.com/wheregmis/threadlane/commit/185fd569a5f120ce3e1cf4a211412e03280e38e8))
* **workspace:** use active git work directory for terminals ([651eb51](https://github.com/wheregmis/threadlane/commit/651eb5162e5d99e161a5cc5ef87d3fcd39d7c3d0))
* **workspace:** use active git work directory for terminals ([a4356b6](https://github.com/wheregmis/threadlane/commit/a4356b6cd610485aecfbe8db2f69b14e99ee2f17))

## [0.1.22](https://github.com/wheregmis/threadlane/compare/v0.1.21...v0.1.22) (2026-09-25)


### CI

* Align release toolchain with Rust 1.95 ([616e6d9](https://github.com/wheregmis/threadlane/commit/616e6d9208be0d9e21bb027b9a320d5c2a952eae))

## [0.1.21](https://github.com/wheregmis/threadlane/compare/v0.1.20...v0.1.21) (2026-09-25)


### Features

* Harden Fusion routing and durability ([3c0e8e7](https://github.com/wheregmis/threadlane/commit/3c0e8e7e0fb283f9c396821c1fb84810c6fbe6b0))
* **provider:** add GPT-6 Sol and Luna models ([ba51eba](https://github.com/wheregmis/threadlane/commit/ba51eba5a414f7dbb05ba3b6c7aa8bdb2caa4541))


### Bug Fixes

* **antigravity:** retry alternate endpoint on forbidden responses ([44b432b](https://github.com/wheregmis/threadlane/commit/44b432bde79a1dd6958c38ca3ade5333378f1a37))
* **browser:** point default URL to the threadlane repository ([af1448e](https://github.com/wheregmis/threadlane/commit/af1448edc9273c1273eb15a5c16a495cdc0efc0f))
* **fusion:** restrict delegation and preserve child lane history ([8e17f6f](https://github.com/wheregmis/threadlane/commit/8e17f6fb760b5eb02d512fcfa88c0bb853ed2827))
* **settings:** display application version in Settings details ([0be16fb](https://github.com/wheregmis/threadlane/commit/0be16fbe2a08fc015d55de4f1854105d48b55fe0))
* **settings:** display application version in Settings details ([e44b990](https://github.com/wheregmis/threadlane/commit/e44b990411f574bdcd2998239c6ad3a1d7ce64ba))
* **ui:** rely on root for automatic overlay rendering ([53a5ac6](https://github.com/wheregmis/threadlane/commit/53a5ac6a59d682fd3948fcd3c09d6835cba61784))


### Build System

* **deps:** bump dtolnay/rust-toolchain from 1.95.0 to 1.120.0 ([46d0251](https://github.com/wheregmis/threadlane/commit/46d02516b0570d5ac34b0da226f1a0ca9536d9de))
* **deps:** bump dtolnay/rust-toolchain from 1.95.0 to 1.120.0 ([cd8f47a](https://github.com/wheregmis/threadlane/commit/cd8f47ad9e303a6fbb82b949faf0087a21f1ac81))
* **deps:** bump gpui-component from `d604a2a` to `ce92671` ([24de007](https://github.com/wheregmis/threadlane/commit/24de00720c181e14461ce8f909fb1df8049aa7a7))
* **deps:** bump gpui-component from `d604a2a` to `ce92671` ([14fab57](https://github.com/wheregmis/threadlane/commit/14fab57aef45ebb0074d1ff8fee849b9bfe4890f))
* **deps:** bump gpui-kit from `d604a2a` to `ce92671` ([b435c5d](https://github.com/wheregmis/threadlane/commit/b435c5dabb798f7b7f14aa00ea7cde7b9ebb9e00))
* **deps:** bump gpui-kit from `d604a2a` to `ce92671` ([6ee72e2](https://github.com/wheregmis/threadlane/commit/6ee72e27c44b976cfbe5e0d2154c32023b618d40))
* **deps:** bump gpui-kit-assets from `d604a2a` to `ce92671` ([310d948](https://github.com/wheregmis/threadlane/commit/310d9482477937be617ba6fe2b5404e0c2f2b8c0))
* **deps:** bump gpui-kit-assets from `d604a2a` to `ce92671` ([6e838e5](https://github.com/wheregmis/threadlane/commit/6e838e56c6df90ad4bf025d5146dc15ca0656618))
* **deps:** bump gpui-wry from `ce92671` to `f8cd486` ([ed4c8fb](https://github.com/wheregmis/threadlane/commit/ed4c8fb0706c18049201610f455fc1747bfed002))
* **deps:** bump gpui-wry from `ce92671` to `f8cd486` ([890ae1f](https://github.com/wheregmis/threadlane/commit/890ae1f95a3a912e14d09ac6f90b9f63758ef8a7))
* **deps:** bump hotpath from 0.25.1 to 0.26.0 ([670d119](https://github.com/wheregmis/threadlane/commit/670d119c089cd8199d896ebd2ada6bd70529ab10))
* **deps:** bump hotpath from 0.25.1 to 0.26.0 ([8c1b8ca](https://github.com/wheregmis/threadlane/commit/8c1b8ca93235232873f56cad46a6d9ed784283ec))


### CI

* use rust 1.95.0 toolchain instead of 1.120.0 ([0134166](https://github.com/wheregmis/threadlane/commit/0134166ae18b494333df12d7f2dc5ed426fdb675))

## [0.1.20](https://github.com/wheregmis/threadlane/compare/v0.1.19...v0.1.20) (2026-09-21)


### Features

* **git:** add staging, commit amend, and stash push support ([8e4f635](https://github.com/wheregmis/threadlane/commit/8e4f6355d0d9da5bba1c2bbf53b492aca0a6868a))
* **git:** add staging, commit amend, and stash push support ([7187cd5](https://github.com/wheregmis/threadlane/commit/7187cd5ad48116b7f33a37f9fcce0aafd09f159d))
* **right-panel:** redesign review toolbar and add diff ratio bar ([dcd0174](https://github.com/wheregmis/threadlane/commit/dcd0174a7ce29b58f165765402e580c1e3e39736))


### Bug Fixes

* **review:** address PR feedback ([a441bef](https://github.com/wheregmis/threadlane/commit/a441befc7799a8e90e764ed53431481729e359a2))

## [0.1.19](https://github.com/wheregmis/threadlane/compare/v0.1.18...v0.1.19) (2026-09-20)


### Features

* **controller:** add scheduler supervisor and durable event APIs ([83099ec](https://github.com/wheregmis/threadlane/commit/83099ec8c24ce62416760843fa3fc85e77609882))
* **controller:** add scheduler supervisor and durable event APIs ([5130823](https://github.com/wheregmis/threadlane/commit/5130823b45d1ebe6fb28aaf96d02ff5e7cb20a7c))


### Bug Fixes

* **git:** parse issue creation URL instead of requesting JSON ([6fbbcc3](https://github.com/wheregmis/threadlane/commit/6fbbcc3082f65d46a6f9642fb1180679f643d51e))
* **git:** parse issue creation URL instead of requesting JSON ([a1568aa](https://github.com/wheregmis/threadlane/commit/a1568aad385daf39b9d2a962ea4aa9256748bc87))

## [0.1.18](https://github.com/wheregmis/threadlane/compare/v0.1.17...v0.1.18) (2026-09-19)


### Features

* act on UI elements by text with computer_interact ([4c67c34](https://github.com/wheregmis/threadlane/commit/4c67c3496c3594dd3d8846e8f78bd53d87b55d29))
* add durable wall time to lifecycle records and run timing ([7fa2034](https://github.com/wheregmis/threadlane/commit/7fa203415983a27c9affb8728ea463425b046c24))
* auto-record trajectories and surface browser routes ([a87bba9](https://github.com/wheregmis/threadlane/commit/a87bba99157952fc1b6fa97db7bfa89be14ed80a))
* **browser:** enhance annotation mode, empty state, and tab interactions ([8c6b457](https://github.com/wheregmis/threadlane/commit/8c6b4579eb7fbc8c1c83cfb1256f54d12de217d2))
* **chat:** add copy, edit, and retry actions with draft safeguards ([139c458](https://github.com/wheregmis/threadlane/commit/139c458aceae588deb75f5644615fdb660fbfd95))
* drive computer use through the CUA driver ([947e3f6](https://github.com/wheregmis/threadlane/commit/947e3f666c33dffa79fd265f7dd724ead6841938))
* expand OpenCode go models and scale chat sizing with rem units ([8c20001](https://github.com/wheregmis/threadlane/commit/8c2000135fe2738c2924e7b3c9f9cb2884174ab6))
* explain mirror stalls and remember panel geometry ([5f950c2](https://github.com/wheregmis/threadlane/commit/5f950c267f1ca274d5bdcedaf574918139a49008))
* GitHub issue create/close/labels/suggest in-app ([f2d732b](https://github.com/wheregmis/threadlane/commit/f2d732beb6e6c9c919a893455562c9f436d3dc99))
* session-scoped computer approval ([9cf6aed](https://github.com/wheregmis/threadlane/commit/9cf6aed78036da0cc4d2b10b994bca729ee567a5))
* **ui:** add composer skills chip and cache acp probe state ([75873e1](https://github.com/wheregmis/threadlane/commit/75873e1357af3a9a2c4d87fda6e79c3264103791))


### Bug Fixes

* ACP forward-compat - lenient plan/tool enums, unknown terminal ends tool, capped tool text, broader question rewrite ([46888e2](https://github.com/wheregmis/threadlane/commit/46888e24294de23562f73cf48112f1805162d464))
* ACP runtime - bounded cancel, reactor-less cancel delivery, orphan tool journal, concurrent preload, advertised effort, question validation ([58a9140](https://github.com/wheregmis/threadlane/commit/58a914022fbee0fa51b52bdfafd23dd03def7132))
* advertise-driven thought-signature gate, keep dropped tool results as text, pass through remote images ([02efac5](https://github.com/wheregmis/threadlane/commit/02efac5c7fdee864711d981728bb788367530444))
* atomic skills save, updater zero-copy install, release lock covers all threadlane crates ([7af319a](https://github.com/wheregmis/threadlane/commit/7af319a881ae2ea78aba380328bba60383952d19))
* browser tab-title test + narrow test imports to avoid macro recursion ([73ecab1](https://github.com/wheregmis/threadlane/commit/73ecab15f6a3b40cced54377ec005b59b90620dc))
* **browser:** hide native webviews when the right panel is hidden ([95ed660](https://github.com/wheregmis/threadlane/commit/95ed660e0d227a7d03d12794d30ab9d1b1bb0d61))
* cache freshness + canonical paths, loop detector hardening, image token accounting ([ad1b7e0](https://github.com/wheregmis/threadlane/commit/ad1b7e06ca9979e90cc1ef149e73cfb2630842da))
* clarity and safety - composer caps, draft/path tooltips, terminal close confirm ([6985244](https://github.com/wheregmis/threadlane/commit/69852444c5c2b70cdfdc753440b86ceeb0dbbdb1))
* **computer:** release feed slot lock before idle re-check ([f996adc](https://github.com/wheregmis/threadlane/commit/f996adce561d9262474ac0834cce4f1b5ea5a592))
* **computer:** track the agent's virtual pointer in the live mirror ([eb249dc](https://github.com/wheregmis/threadlane/commit/eb249dc061c19a8e420150420a5b0a6abe4faeee))
* **computer:** track the agent's virtual pointer in the live mirror ([ad399e9](https://github.com/wheregmis/threadlane/commit/ad399e940b0b62218c6d8f0541a12e97fdc7ca24))
* effort-aware prewalk noop fast path, live requires_todo refresh ([b7ae3f4](https://github.com/wheregmis/threadlane/commit/b7ae3f4fca0f10008073962c4adddd4b13976bd2))
* exclude inline image bytes from token estimates ([99500f1](https://github.com/wheregmis/threadlane/commit/99500f189378666d27b536caa2fae3a627a4cc7a))
* GitHub list resilience - skip bad entries, surfaced review errors, rate-limit backoff ([5fc1c35](https://github.com/wheregmis/threadlane/commit/5fc1c358857b4a62fd8a90da477f8c13f0ae768f))
* GitHub toolbar shares the header clearance token ([f2535c2](https://github.com/wheregmis/threadlane/commit/f2535c26052d60f00c988c76080a31a8377ddde5))
* harness identity - RunContext attempt suffix, (run,call) tool ids, consecutive-only lane collapse, canonical hub key ([29c616a](https://github.com/wheregmis/threadlane/commit/29c616ae110214885121b60dc0c34ff53c1dee81))
* interface nits - unified empty state, slash menu rhythm, pill scrim, compact empties ([a2be57a](https://github.com/wheregmis/threadlane/commit/a2be57a694b2a9a861e9b9c5e194e450f050fb65))
* keyboard/a11y and docs honesty - question send guard, distinct palette icons, real shortcuts ([cf8347f](https://github.com/wheregmis/threadlane/commit/cf8347f1cac0770958ef3d39bb770db3f1f0e255))
* live discovery merges into registry with TTL; project roots threaded to budgets ([12026f8](https://github.com/wheregmis/threadlane/commit/12026f8924627e75816b3fb6dd0653e5581f9103))
* MCP multiplex + singleflight, editor/sidebar off UI thread, auth reactor hop, incremental memory validation ([3dc3b13](https://github.com/wheregmis/threadlane/commit/3dc3b1316fdeef39b279913653da37a7f4fcf329))
* registry newer-wins merge + subagent worktree orphan reclamation ([1d014ce](https://github.com/wheregmis/threadlane/commit/1d014ce196f11270c125b1c4130d7c124efd2989))
* render/event/launch paths degrade instead of panic ([5fd584d](https://github.com/wheregmis/threadlane/commit/5fd584ddc98d348ecae6470731ba1d3deda4924f))
* retry transient OpenCode errors and avoid effort state overwrite ([0e1e75c](https://github.com/wheregmis/threadlane/commit/0e1e75c6d308c48dc20b1f3f7f091b0e4b20418c))
* sidebar rows announce, archive discoverable, honest dialog copy ([08c006d](https://github.com/wheregmis/threadlane/commit/08c006d3e705ab7de7a657b8540d25d623330bda))
* subagent hub - system kill notices, late broadcast replay, solo follow-ups, bounded inbox ([15018a5](https://github.com/wheregmis/threadlane/commit/15018a5c3f4131919c8b19621ea1ea3cb1b3dd51))
* surface swallowed errors; poison-tolerant session locks ([4e3d4ea](https://github.com/wheregmis/threadlane/commit/4e3d4eacdd5be70c7a6babc2dff1a81a38f2c82a))
* title routing skips antigravity/acp; fallback roster rebuilds on rotation ([65009f4](https://github.com/wheregmis/threadlane/commit/65009f4611af3903cbf8097856fbb017e3e6ef1c))
* **ui-chat:** isolate progress summary tool status to current turn ([c80cb89](https://github.com/wheregmis/threadlane/commit/c80cb89fd269c5340b06547ee6930168bc329aed))
* **ui:** improve accessibility labels and review workflow feedback ([c9c5cd9](https://github.com/wheregmis/threadlane/commit/c9c5cd90939540ba3eab27f350a050efca1f3261))
* **ui:** move progress summary below transcript and allow effort change ([24bab6c](https://github.com/wheregmis/threadlane/commit/24bab6cac6f6d487906231a3dd217e2358ea42a4))
* updater bundle allowlist; remove_session archives + dirty guard; surfaced cleanup errors ([b01f568](https://github.com/wheregmis/threadlane/commit/b01f568635baa7a4af40a9e4103fb33ddef71379))
* validate computer window targets and prune stale mirror keys ([9a6d09f](https://github.com/wheregmis/threadlane/commit/9a6d09f4423d8ec8acb6ef4c77484f60dcb81f3e))
* WASI state/scope/alloc safety + broker framing caps ([1550035](https://github.com/wheregmis/threadlane/commit/1550035872429bb044ac7061cab0065a9d43b356))


### Performance Improvements

* **git:** gate github requests and improve caching with backoff ([2092432](https://github.com/wheregmis/threadlane/commit/209243242ed6d6bc6e03ab151422c8e394d6e865))

## [0.1.17](https://github.com/wheregmis/threadlane/compare/v0.1.16...v0.1.17) (2026-09-18)


### Features

* **auth:** Add injectable credential storage ([e483c61](https://github.com/wheregmis/threadlane/commit/e483c615b8906b2394690a0fe1535c5f8e28fc13))
* **ui:** add environment sidebar to chat and refine github view layout ([f1f4437](https://github.com/wheregmis/threadlane/commit/f1f4437149b1366acb74c99468a7fb287c8556bb))
* **ui:** Improve navigation and accessibility ([1b43ad9](https://github.com/wheregmis/threadlane/commit/1b43ad9f513f0afcb4a899e94dcec3e735bbb4fc))


### Bug Fixes

* **acp:** Dispatch assistant hooks for ACP turns ([47c5fe1](https://github.com/wheregmis/threadlane/commit/47c5fe1ca360b08bad1349b4e754de43fa600fc8))


### Code Refactoring

* **acp:** Extract protocol into standalone crate ([061577f](https://github.com/wheregmis/threadlane/commit/061577f787f121b752ca4bf631e7105dde8e1f6e))
* **auth:** Inject credential storage ([38e3d55](https://github.com/wheregmis/threadlane/commit/38e3d5505ab78fb595f5920c460c21a85cabd831))
* Centralize shared tools and contracts ([bd7f764](https://github.com/wheregmis/threadlane/commit/bd7f7643776d71a97400da778fe021cf9de3c581))
* Decouple MCP client from runtime ([3bb9581](https://github.com/wheregmis/threadlane/commit/3bb95812aeb5453ed9db4132a8510299cf831649))
* Decouple skills and expose hashline APIs ([610a6ab](https://github.com/wheregmis/threadlane/commit/610a6ab4ca76fca6841b6ab5138362b642805392))
* Extract shared modules from session ([5d67c38](https://github.com/wheregmis/threadlane/commit/5d67c3823eed727f59f6b9b03593de7f93c64b06))
* Move browser bridge to protocol ([4e886a9](https://github.com/wheregmis/threadlane/commit/4e886a98a00c8001cc314db9c99f578c4e800c9c))
* Move prewalk orchestration to runtime ([894be5c](https://github.com/wheregmis/threadlane/commit/894be5c726cf667f0067c28570bd687c1fdc3d4f))
* Move provider contracts out of runtime ([24dac1e](https://github.com/wheregmis/threadlane/commit/24dac1ee41c6763d6699f3340364b9c03900ec4b))
* Move ToolPolicy into runtime crate ([6de220d](https://github.com/wheregmis/threadlane/commit/6de220d7e3944900bb509ab1c0623a5db417c1bf))
* Restrict internal API visibility ([5be05db](https://github.com/wheregmis/threadlane/commit/5be05dbea92c589b4b9f508114465dca82343ee2))
* **session:** Simplify module organization ([0a95727](https://github.com/wheregmis/threadlane/commit/0a95727597e10c4c75965eb80bdeff65503c15f7))
* **tools:** Inject remote forge credentials ([b489b8a](https://github.com/wheregmis/threadlane/commit/b489b8a39a32961932b794dc98be4543a2e73e06))


### CI

* skip draft PRs and run checks when ready for review ([c3faf75](https://github.com/wheregmis/threadlane/commit/c3faf75c8d4c8ec5e1bb320984c57549360d2d78))


### Maintenance

* configure nightly Rust with parallel compilation ([ff57a65](https://github.com/wheregmis/threadlane/commit/ff57a650523b515e0c4dad4bfa216326fab74005))
* deduplicate workspace dependencies into root cargo.toml ([445e6bc](https://github.com/wheregmis/threadlane/commit/445e6bcdd139b34d41e745734d3501f4b7cf70c8))
* **deps:** bump gpui-component from `382fc28` to `d604a2a` ([2aaf671](https://github.com/wheregmis/threadlane/commit/2aaf671c381d6bc95deb5ffb9dca9a04124b4036))
* **deps:** bump gpui-component from `382fc28` to `d604a2a` ([ac9a7fc](https://github.com/wheregmis/threadlane/commit/ac9a7fcfddb1c77103a361db868f83c353e1bcf2))
* **deps:** bump gpui-kit from `382fc28` to `d604a2a` ([495d001](https://github.com/wheregmis/threadlane/commit/495d001a4ee87b381fc06d5f3f4989b3a6f2608f))
* **deps:** bump gpui-kit from `382fc28` to `d604a2a` ([948eb8a](https://github.com/wheregmis/threadlane/commit/948eb8a930454f04755442934ab0fb8ff5f0c51b))
* **deps:** bump gpui-kit-assets from `382fc28` to `d604a2a` ([814170b](https://github.com/wheregmis/threadlane/commit/814170b94de399797c86a952328026814da21937))
* **deps:** bump gpui-kit-assets from `382fc28` to `d604a2a` ([c9b0e1f](https://github.com/wheregmis/threadlane/commit/c9b0e1fae3f11ea30e4b4b63109fe970e76642fa))
* **deps:** bump notify from 7.0.0 to 8.2.0 ([789539c](https://github.com/wheregmis/threadlane/commit/789539c8e8201de381b48a6593cb4a03770e8930))
* **deps:** bump notify from 7.0.0 to 8.2.0 ([a5fcdd1](https://github.com/wheregmis/threadlane/commit/a5fcdd1f1bc8522ad43bb7cf9c5dad8f9f303c1e))
* **deps:** bump robius-open from `9ca3f2d` to `71966dc` ([c68f223](https://github.com/wheregmis/threadlane/commit/c68f2236c8e0e0c23dc86192171e9b5f6247b0b5))
* **deps:** bump robius-open from `9ca3f2d` to `71966dc` ([7c3f913](https://github.com/wheregmis/threadlane/commit/7c3f9139e09869dff404d7f6b7f5ec572e35205c))
* Document provider layer boundaries ([8114c85](https://github.com/wheregmis/threadlane/commit/8114c855dde6716a2790d6f18128adf07305d596))

## [0.1.16](https://github.com/wheregmis/threadlane/compare/v0.1.15...v0.1.16) (2026-09-11)


### Features

* follow conventional commits format for generated commit messages ([728a15d](https://github.com/wheregmis/threadlane/commit/728a15df207e6ce0d2fb7ba84651f7ed655c3f32))
* follow conventional commits format for generated commit messages ([e4a70b7](https://github.com/wheregmis/threadlane/commit/e4a70b73adc72aed874c5628c2652ac205d4fe17))
* **gpui:** float the computer-use mirror inside the chat view ([ea27e83](https://github.com/wheregmis/threadlane/commit/ea27e839ac0324c5faa2388ac2561cad67153bd3))


### Bug Fixes

* address dynamic model review feedback ([744a216](https://github.com/wheregmis/threadlane/commit/744a2167f2243e7cf523c93ef5aba19a506d2c19))
* **gpui:** keep the floating mirror inside the host and release its frames ([a6451dd](https://github.com/wheregmis/threadlane/commit/a6451dd47ff150c1debcdbc97143abb5f7b012d2))
* **gpui:** keep wasmi off GPUI's 512 KiB worker stacks during hydration ([3fd90f8](https://github.com/wheregmis/threadlane/commit/3fd90f819823585f742e853406f2898ffe8a0bde))
* preserve effort for draft and background sessions ([9ff57f7](https://github.com/wheregmis/threadlane/commit/9ff57f76d9e56824bf6140fe7c1ebc2e36973b2c))
* preserve model and reasoning across sessions ([1ceae22](https://github.com/wheregmis/threadlane/commit/1ceae221d52d1b88a0168b74221e4784aea3f4f1))
* preserve session reasoning effort ([6cca1fe](https://github.com/wheregmis/threadlane/commit/6cca1fe20cfe2150e19b8023bca4cb22919325aa))
* **router:** address review feedback for commit message normalization ([85b9708](https://github.com/wheregmis/threadlane/commit/85b9708ec12b5451342d0b29f6cef302805ef41a))

## [0.1.15](https://github.com/wheregmis/threadlane/compare/v0.1.14...v0.1.15) (2026-09-11)


### Features

* **session:** add macOS capture resolution modes ([8c836c2](https://github.com/wheregmis/threadlane/commit/8c836c2ec5d74d9f07ce0bb64d5e7345f860e907))

## [0.1.14](https://github.com/wheregmis/threadlane/compare/v0.1.13...v0.1.14) (2026-09-10)


### Features

* add recent commands and linked GitHub issue context ([88f4400](https://github.com/wheregmis/threadlane/commit/88f440047b2aed3382434e96ef0348a8cc9117e0))
* add recent commands and linked GitHub issue context ([69cba32](https://github.com/wheregmis/threadlane/commit/69cba32df8eee06532142bcf194fd6569c24eef1))
* bug fixes ([09899b4](https://github.com/wheregmis/threadlane/commit/09899b4420a482aa92050a0e60029f740619bf51))
* bug fixes ([26e67e0](https://github.com/wheregmis/threadlane/commit/26e67e05ecd2983e7dc3e903a79fb053d8440b10))
* **chat:** add session attention badges, progress summary, and caching improvements ([e682066](https://github.com/wheregmis/threadlane/commit/e682066628bd02783ddfadf7d31b3601182bf5cf))


### Bug Fixes

* support GitHub Enterprise hosts for remote refs ([a3bf934](https://github.com/wheregmis/threadlane/commit/a3bf9348c5aa6715188dabc90b7ca4b0369db784))


### Maintenance

* refactoring alot of stuffs ([cccc100](https://github.com/wheregmis/threadlane/commit/cccc100437de178a9c76d2d81b2710f7aff0eb19))

## [0.1.13](https://github.com/wheregmis/threadlane/compare/v0.1.12...v0.1.13) (2026-09-10)


### Features

* add automatic PR review feedback addressing ([0100fa6](https://github.com/wheregmis/threadlane/commit/0100fa68ca6144ba0196847c4d59b42c01358b6a))
* **gpui:** add OpenCode and Antigravity ACP presets ([2e288e2](https://github.com/wheregmis/threadlane/commit/2e288e21129554eb9e1693537b6dd88515109e98))
* **gpui:** normalize child process PATH on macOS ([6cf4cbc](https://github.com/wheregmis/threadlane/commit/6cf4cbc0f46df049446aea75e6b99e77ffea1d67))


### Bug Fixes

* dispatch PR review feedback to linked sessions ([1432339](https://github.com/wheregmis/threadlane/commit/1432339840ddba5030526ef8d4fbc424676b0685))
* finish extension runs before ACP follow-ups ([beca9e3](https://github.com/wheregmis/threadlane/commit/beca9e37461e60ee0e42a8f6c39615ceecad7832))
* **git:** show git action feedback as notifications ([8ee1ce8](https://github.com/wheregmis/threadlane/commit/8ee1ce81ce5f4013976a60cb79f9634deecd88f3))
* **gpui:** harden PR review automation ([079babe](https://github.com/wheregmis/threadlane/commit/079babe8f2f8d977e5e8d72b848ed7b175db4259))
* **gpui:** persist PR feedback after dispatch ([5b36deb](https://github.com/wheregmis/threadlane/commit/5b36deb64d1659332188b470e412e5fb1d62512d))
* **gpui:** preserve system PATH when unset ([1c19c00](https://github.com/wheregmis/threadlane/commit/1c19c00cec1b65cbc1a924505f33e82664700c52))
* initialize child process PATH ([25045b9](https://github.com/wheregmis/threadlane/commit/25045b957ea00b4db99289448d91d18cd4426813))
* preserve subagent worktree isolation ([dbb20c3](https://github.com/wheregmis/threadlane/commit/dbb20c37d8b6852a11e988a37e3663747ca72527))
* route /goal follow-ups through ACP agents ([ab842d9](https://github.com/wheregmis/threadlane/commit/ab842d91efdf6563b6779ed509b96ddb43649292))
* route goal follow-ups through ACP agents ([1eccf21](https://github.com/wheregmis/threadlane/commit/1eccf21ddd9a2f32d089fe6f49f8357094068868))
* **sidebar:** show merge icon for merged pull requests ([a1bb64d](https://github.com/wheregmis/threadlane/commit/a1bb64def9d3a8dd38b84e300ce48f0d8e2ed1d2))


### Maintenance

* **deps:** bump hotpath from 0.24.0 to 0.25.1 ([e7f1134](https://github.com/wheregmis/threadlane/commit/e7f1134a529682d979dd38f3637b021225c8dc56))

## [0.1.12](https://github.com/wheregmis/threadlane/compare/v0.1.11...v0.1.12) (2026-09-09)


### Features

* **github:** automate issue branch push and draft PR creation ([50c88b1](https://github.com/wheregmis/threadlane/commit/50c88b192011a9441995f2b5c59ee1ac7c3a82fa))
* **github:** automate issue branch push and draft PR creation ([06f0a13](https://github.com/wheregmis/threadlane/commit/06f0a13aa60765175cb8e4d14357bb3ee1f4fe9c))
* **gpui:** preload ACP agent models and polish session archive button ([480364d](https://github.com/wheregmis/threadlane/commit/480364dda8206f9a2f28d7487cf20ce4b00fc176))
* **gpui:** preload ACP agent models and polish session archive button ([aede862](https://github.com/wheregmis/threadlane/commit/aede8627fe822ef3596264722dc9ea4dfc2d9a67))


### Bug Fixes

* **github:** safely publish issue draft PRs ([8ff994c](https://github.com/wheregmis/threadlane/commit/8ff994c1f7309392b815b3b7c84d00ffd15a6fd2))
* **gpui:** make ACP model picker scrollable ([658120b](https://github.com/wheregmis/threadlane/commit/658120b757395382cc45b640fa6f04dd5456a543))
* **gpui:** unify tooltips, header tokens, and type scale ([d1c56a6](https://github.com/wheregmis/threadlane/commit/d1c56a671dcb7c30f991472a4e54d017e057eb5b))


### Maintenance

* **deps:** bump core-graphics from 0.24.0 to 0.25.0 ([b40a75d](https://github.com/wheregmis/threadlane/commit/b40a75da7af54eafc93a1123b3588ff8cda43cf1))
* **deps:** bump core-graphics from 0.24.0 to 0.25.0 ([55f22dc](https://github.com/wheregmis/threadlane/commit/55f22dc26266988705862a3e1210fcc04cefe032))
* **deps:** bump dirs from 6.0.0 to 7.0.0 ([80f9eb0](https://github.com/wheregmis/threadlane/commit/80f9eb05f02d96ebfadc47af6f2671c4c4d99c08))
* **deps:** bump dirs from 6.0.0 to 7.0.0 ([d93e6ec](https://github.com/wheregmis/threadlane/commit/d93e6ec66f0386c691d1df616525845e1ce2b4da))
* **deps:** bump gpui-component from `4833601` to `382fc28` ([29138a3](https://github.com/wheregmis/threadlane/commit/29138a367a19fcbcbe0eae7e029a7c50d1adc775))
* **deps:** bump gpui-component from `4833601` to `382fc28` ([a3423be](https://github.com/wheregmis/threadlane/commit/a3423be359df308799e9378e00cec2d2838fda8d))
* **deps:** bump wasmi from 1.1.0 to 2.0.0 ([9e04359](https://github.com/wheregmis/threadlane/commit/9e0435969a8796fde998ab5c9c8618afd0f17405))
* **deps:** bump wasmi from 1.1.0 to 2.0.0 ([32c13a0](https://github.com/wheregmis/threadlane/commit/32c13a0e08127451c00cac76095c39aa0bf800a7))

## [0.1.11](https://github.com/wheregmis/threadlane/compare/v0.1.10...v0.1.11) (2026-09-09)


### Features

* Add browser tools and question controls ([f8d00e9](https://github.com/wheregmis/threadlane/commit/f8d00e93028d4ea0239fbd6aac060920b2a047ba))
* Add live computer-use mirror ([286c7c5](https://github.com/wheregmis/threadlane/commit/286c7c5e0871a0876edded682b9b6d7b5977d97a))
* Attach screenshots to tool results ([45b42a9](https://github.com/wheregmis/threadlane/commit/45b42a9f48f40d3134160aff33278ed063bf70fa))
* Computer Use and Embedded Browser ([a9bc1fa](https://github.com/wheregmis/threadlane/commit/a9bc1fafb25dd295541b9a4371aca4991efeb207))
* **computer:** Add live frame polling ([236865c](https://github.com/wheregmis/threadlane/commit/236865cfb1a6436754767ee740cefd050e85d691))
* **github:** Add project-scoped issue views ([3ce0366](https://github.com/wheregmis/threadlane/commit/3ce0366b7a27e74ae0a486ba6432f208ae930d64))
* **gpui:** Improve control tooltips and loading states ([a0a9755](https://github.com/wheregmis/threadlane/commit/a0a9755a64c967909ec37a76a768457b7b349986))
* **runtime:** Add tool loop guardrails ([9129d28](https://github.com/wheregmis/threadlane/commit/9129d2850fa338fa0e507d44453194e8d6196514))
* **runtime:** Cache repeated tool reads per turn ([3cc4e47](https://github.com/wheregmis/threadlane/commit/3cc4e47a4514803ee3bc06b5654bf0eb684d7881))
* **runtime:** Preserve failures and bust caches ([9bab533](https://github.com/wheregmis/threadlane/commit/9bab5334a129600c3b1966d69d07217b3c09de66))
* Support targeted background computer input ([7a9c386](https://github.com/wheregmis/threadlane/commit/7a9c386120c8d9326173c68bcecbda350a9c84f0))


### Bug Fixes

* clarify computer-use approval and verification guidance ([346bc10](https://github.com/wheregmis/threadlane/commit/346bc1011b3d49b4ed029b1f4a880622518d74bc))
* **gpui:** improve empty states and accessibility ([24809df](https://github.com/wheregmis/threadlane/commit/24809df6cd2fedd4dbbee69c69da589242d25843))
* Improve computer-use capture handling ([6904b25](https://github.com/wheregmis/threadlane/commit/6904b2505512f9fb9b3aa033cd72ec6a497b51ec))
* Preserve update notice dismissal state ([35084e5](https://github.com/wheregmis/threadlane/commit/35084e5955e193d81ea408f7aa329959dfdee4f6))

## [0.1.10](https://github.com/wheregmis/threadlane/compare/v0.1.9...v0.1.10) (2026-09-07)


### Features

* Add configurable prewalk orchestration ([ecb1958](https://github.com/wheregmis/threadlane/commit/ecb195820dbd658a0e8dcf39ea7be3da2e8a7845))
* add GitHub issue workspace ([3130e2f](https://github.com/wheregmis/threadlane/commit/3130e2f9fdeb0b120ab925d0c61e98d9e68d89d3))
* add on-demand context snapshots ([635d1b2](https://github.com/wheregmis/threadlane/commit/635d1b2fca96cd7a869ab3320e97325cf1bbe987))
* Add prewalk workflow and fast model support ([817df34](https://github.com/wheregmis/threadlane/commit/817df34e7206ced43c26a9d2c19cf3a822d29061))
* Add slash menu and permission details ([90888e6](https://github.com/wheregmis/threadlane/commit/90888e6677be3b00e501461755e00e4bc06fabd9))
* Add slash menu and permission details ([aaaf7b4](https://github.com/wheregmis/threadlane/commit/aaaf7b4e22de4321badfb4144ec5602fdb59fec2))
* add typed GitHub workflow contracts ([a6ec89e](https://github.com/wheregmis/threadlane/commit/a6ec89e89ec223ee84e8788c9947315aec17e5cb))
* Advance Github UI ([6f29877](https://github.com/wheregmis/threadlane/commit/6f298771887b600935c5ee36f0e0ac4731f53a7b))
* create editable draft pull requests ([ede1b07](https://github.com/wheregmis/threadlane/commit/ede1b07ce0c3b74e462afd90329ca73d72151590))
* draft and publish PR comments ([9a48ff8](https://github.com/wheregmis/threadlane/commit/9a48ff8c20b092aab8e9ccd18334823a8742192b))
* draft and publish PR review replies ([36ccc3b](https://github.com/wheregmis/threadlane/commit/36ccc3b8b3bb2dd7080bb17d6bbd9e2e6e0d55c6))
* **git:** add pull request creation support ([30faa30](https://github.com/wheregmis/threadlane/commit/30faa300a3a2b5f63490c73347fc931440fb5f37))
* **github:** Show PR commits and pending messages ([a8f37b0](https://github.com/wheregmis/threadlane/commit/a8f37b002eebb3adaabf226cb8a4ac82534eaba8))
* **gpui:** Add detailed pull request tooltips ([37ad11d](https://github.com/wheregmis/threadlane/commit/37ad11d89b914b4fb9c1931a438979741811be6e))
* **gpui:** Improve slash command completion navigation ([c3643f5](https://github.com/wheregmis/threadlane/commit/c3643f510a8c6d3dec398987e73bd8b18c41768e))
* Improve GitHub filters and agent guidance ([4ad2662](https://github.com/wheregmis/threadlane/commit/4ad266205f2910164a2ab72dc0257b0f47026f67))
* inspect pull requests in GitHub workspace ([61446e6](https://github.com/wheregmis/threadlane/commit/61446e66103f5898a2470e896f14f21ed65cd336))
* link GitHub issues to isolated tasks ([85c92b0](https://github.com/wheregmis/threadlane/commit/85c92b0a0a0062f9e6df33d3c67b0c0ae8932eca))
* Option to create PR from review ([07aee74](https://github.com/wheregmis/threadlane/commit/07aee740319a6366985fde4c064575a1859d8225))
* report context snapshot reuse ([4df5cec](https://github.com/wheregmis/threadlane/commit/4df5cec1b0f70f71fa137b8945200cbb134525bd))
* **runtime:** index durable context snapshots ([3f16dee](https://github.com/wheregmis/threadlane/commit/3f16deea5f2631d929f1be43115da2ce2d1882ee))
* **session:** capture read context snapshots ([5385a15](https://github.com/wheregmis/threadlane/commit/5385a156a220d819cbc21624552e251c3b48b8e4))
* **session:** load durable context on demand ([2192180](https://github.com/wheregmis/threadlane/commit/2192180b9513be61b3ced4ab6d4e3957c6561cad))
* **session:** pass selected context to subagents ([dc96425](https://github.com/wheregmis/threadlane/commit/dc9642583a14b74a4b8961d62859ddf9da0a064b))
* **session:** retain context index through compaction ([8239d98](https://github.com/wheregmis/threadlane/commit/8239d9872795bf433de7c9aa0022eaaabde12bf8))
* start issue tasks from GitHub ([a5c524f](https://github.com/wheregmis/threadlane/commit/a5c524ff4fe44c7ec40bee8a1dc0aad3803bbf62))
* surface background agent attention ([f70301e](https://github.com/wheregmis/threadlane/commit/f70301e59dc3a5f50f12c7fe4bafcbb1ba32281d))


### Bug Fixes

* complete issue task confirmation UX ([0e9a0f9](https://github.com/wheregmis/threadlane/commit/0e9a0f99d0e041d379b014c000eac44d2d691b39))
* **context:** close snapshot review gaps ([64b7aba](https://github.com/wheregmis/threadlane/commit/64b7aba1331d2f4f618defbf478055a572cb3aae))
* **context:** delimit compacted snapshot indexes ([46528e9](https://github.com/wheregmis/threadlane/commit/46528e9a01d42644c12d15907e5a62ecf889db32))
* **context:** harden durable snapshot reuse ([b2c478b](https://github.com/wheregmis/threadlane/commit/b2c478b26e4ca557dab9506f124775cfdf2aed87))
* **context:** omit indexed outputs by entry id ([c2a8b76](https://github.com/wheregmis/threadlane/commit/c2a8b76ffcd1bc82c41f82124705c55a54490d33))
* **context:** preserve malformed snapshot indexes ([a3e5632](https://github.com/wheregmis/threadlane/commit/a3e56321eb43c2771af33e6ac14f747982279a7b))
* **context:** preserve structured index across compaction ([8a9903d](https://github.com/wheregmis/threadlane/commit/8a9903d114251f734da3ec9e517ac99d12398510))
* **context:** stabilize repeated snapshot indexing ([80005e2](https://github.com/wheregmis/threadlane/commit/80005e22b0566ff3ce86abb57ec1d64f903f3f00))
* Correct GitHub reviews and workspace state ([599b1ed](https://github.com/wheregmis/threadlane/commit/599b1ed236dafd2d51052f847d179985de057548))
* Deduplicate and resolve cross-project sessions ([703b172](https://github.com/wheregmis/threadlane/commit/703b172c660cc9e73de3e5be939b2a6f39181312))
* **github:** Render comments as markdown ([e4ae1b2](https://github.com/wheregmis/threadlane/commit/e4ae1b25c35eeecde0a3e39a1da961ebf3483e03))
* **gpui:** Reset task directory and status ([355b7ee](https://github.com/wheregmis/threadlane/commit/355b7ee2a42a248210c127eda56b731a203031a0))
* Handle subagent usage and archive clicks ([0353d27](https://github.com/wheregmis/threadlane/commit/0353d2755d889c58b22283bb988abfec0e20a05d))
* Harden fuzzy workspace path resolution ([be3fd70](https://github.com/wheregmis/threadlane/commit/be3fd70215abdf44b37afac8f0246f2a866e14ee))
* harden GitHub workflow contracts ([6616f0c](https://github.com/wheregmis/threadlane/commit/6616f0c32d0a1c3c239efad4731e8131ec624ca0))
* harden GitHub workspace refreshes ([55cf18c](https://github.com/wheregmis/threadlane/commit/55cf18c264b07c08f369febd3e5bce4defa39458))
* harden pull request inspection UX ([b33dee3](https://github.com/wheregmis/threadlane/commit/b33dee3407f115b13f1cfb865038f6508cfacd1e))
* Improve project filtering and recovery ([965918c](https://github.com/wheregmis/threadlane/commit/965918ce6ae65d85cae4e2cfb37f6dd977b6f11b))
* keep disabled issue dialog open ([863cf55](https://github.com/wheregmis/threadlane/commit/863cf555b359f3a254f4200738ebc193d4d4f6d3))
* keep editor worktree targets scoped ([7a2f8fe](https://github.com/wheregmis/threadlane/commit/7a2f8fe776727cbf800ac7ea54bb98b1c90f0866))
* make PR recovery keyboard reachable ([230aac7](https://github.com/wheregmis/threadlane/commit/230aac72fe0bee95aa8bd8d1ca973077d4dbe2ca))
* make pull request reply recovery safe ([8fbb703](https://github.com/wheregmis/threadlane/commit/8fbb703d537b64921944c408a04bc1e5662db728))
* preserve newer PR comment drafts ([d2993e1](https://github.com/wheregmis/threadlane/commit/d2993e108fa6b734230f3fd0018243eae584ac8a))
* preserve PR review context and diff state ([9444475](https://github.com/wheregmis/threadlane/commit/944447570d26b97633dd1614d743c8e768536a6d))
* preserve session metadata and stale git operation state ([40847b8](https://github.com/wheregmis/threadlane/commit/40847b87398aff2a75c7395da92c5ba05245e2af))
* Refine PR and subagent context handling ([bd399e3](https://github.com/wheregmis/threadlane/commit/bd399e3e29ef19cbf08dcf0b4aa34cd61711a707))
* Require JSON arguments for dyn tools ([931a611](https://github.com/wheregmis/threadlane/commit/931a6113c0c8068c53ec563e41ba67ece4a1ecf2))
* Restore slash command scrollbar rendering ([c7f5517](https://github.com/wheregmis/threadlane/commit/c7f5517a66ae299ead67d34b01ac85d69ffc973c))
* retain GitHub token for error redaction ([fac1e61](https://github.com/wheregmis/threadlane/commit/fac1e619ff161c1bcdd90aef6dd9e7b44cca9f03))
* roll back failed issue work startup ([acdb256](https://github.com/wheregmis/threadlane/commit/acdb256e91347ab5146f9a2ff08b32da540f1153))
* route Git actions to session worktrees ([032b4ca](https://github.com/wheregmis/threadlane/commit/032b4caadbbca23b6590a22403eb3646e9b0b46c))
* **session:** handle unicode snapshot paths ([b7d3e69](https://github.com/wheregmis/threadlane/commit/b7d3e69636a02217e70742a1a72f9dda8d22463c))
* **session:** reject remote snapshot paths ([59f90d0](https://github.com/wheregmis/threadlane/commit/59f90d02c572c70c700db48fdd7cfcf19fe4701c))
* **session:** validate context snapshot projection ([fa0e72a](https://github.com/wheregmis/threadlane/commit/fa0e72a8de6a88d93e99d5f0c56a0355e28fc16c))
* Show active session project in composer chip ([70a31f7](https://github.com/wheregmis/threadlane/commit/70a31f7fb31f72df477ebeaa78f817005afe2abf))
* Show unavailable worktrees ([a008aff](https://github.com/wheregmis/threadlane/commit/a008aff3f56afc4f6a86037157b45c856d6b4625))
* Show worktree status in sidebar tooltip ([568182f](https://github.com/wheregmis/threadlane/commit/568182fdf8e8c2bf4f598b4f603cd56b395fd35a))
* Stop Animating Context Meter for Unreported Usage ([4b9077a](https://github.com/wheregmis/threadlane/commit/4b9077a1ecd6ec99b46482d5a639c9f99adae4c5))
* **ui:** refine chat shortcuts and merged PR styling ([4f91e9e](https://github.com/wheregmis/threadlane/commit/4f91e9e4d755202a3717b96b6ff94f1910030a97))
* Validate worktrees and support dyn tools ([06f979e](https://github.com/wheregmis/threadlane/commit/06f979e9e376ec60ef7af2a1f69846b64515a6e6))
* **worktree:** separate project and runtime session directories ([2c704e5](https://github.com/wheregmis/threadlane/commit/2c704e514e086b8abc18c52f787d215b060df23d))


### Code Refactoring

* **session:** Slim prompts and defer context ([b6e2ef9](https://github.com/wheregmis/threadlane/commit/b6e2ef98d1760aafcc8aea9989e6e52e9b5c03cd))


### Maintenance

* **deps:** bump actions/checkout from 4 to 7 ([27d95d5](https://github.com/wheregmis/threadlane/commit/27d95d500e6a34ec72e1eba37eb30f6ec2151d0c))
* **deps:** bump actions/checkout from 4 to 7 ([f6fe2fa](https://github.com/wheregmis/threadlane/commit/f6fe2fa510b277621d10b24829b7ce99e8492623))
* **deps:** bump actions/download-artifact from 4.3.0 to 8.0.1 ([6280d88](https://github.com/wheregmis/threadlane/commit/6280d8825d7fac3608154daa8c203e3ff24d8d86))
* **deps:** bump actions/download-artifact from 4.3.0 to 8.0.1 ([b640f1f](https://github.com/wheregmis/threadlane/commit/b640f1f1a3d4843b1b7f81bd369203227084e941))
* **deps:** bump actions/upload-artifact from 4.6.2 to 7.0.1 ([da6797b](https://github.com/wheregmis/threadlane/commit/da6797bf10c306ad8cf52e799237c94969b0a1e7))
* **deps:** bump actions/upload-artifact from 4.6.2 to 7.0.1 ([934293b](https://github.com/wheregmis/threadlane/commit/934293bb133ace2cc0a8841188a8ca4ad9994383))
* **deps:** bump gpui from `4ccbcab` to `a61e260` ([efbd4cf](https://github.com/wheregmis/threadlane/commit/efbd4cf1a7814470a7466007b5ccdd3bf6447bde))
* **deps:** bump gpui from `4ccbcab` to `a61e260` ([85f707c](https://github.com/wheregmis/threadlane/commit/85f707cdd10e69c8e8230ba56163be6eb9ae8f1a))
* **deps:** bump gpui from `a61e260` to `206a863` ([e885d92](https://github.com/wheregmis/threadlane/commit/e885d92b43c1ee0aa0ce06b47e82526177ada355))
* **deps:** bump gpui from `a61e260` to `206a863` ([9d3a782](https://github.com/wheregmis/threadlane/commit/9d3a782db0503b792438cb0efcd7dc56158ae01d))
* **deps:** bump gpui_platform from `4ccbcab` to `a61e260` ([e155e5e](https://github.com/wheregmis/threadlane/commit/e155e5e656d611482a79d054786bfdad3a405570))
* **deps:** bump gpui_platform from `4ccbcab` to `a61e260` ([44f0e62](https://github.com/wheregmis/threadlane/commit/44f0e6214c78a069ec945d0c1ffcc6be7fd7964e))
* **deps:** bump gpui_platform from `a61e260` to `206a863` ([58f2106](https://github.com/wheregmis/threadlane/commit/58f2106e6a0c9b4a5b2b563bbdf5b1746a259eba))
* **deps:** bump gpui_platform from `a61e260` to `206a863` ([a932994](https://github.com/wheregmis/threadlane/commit/a93299440be8dd10ec383954cd663666a6561d5e))
* **deps:** bump gpui-component from `ff3eb11` to `0e2fb7a` ([bcb15a2](https://github.com/wheregmis/threadlane/commit/bcb15a249997033289141f1076cb1b43b081fd42))
* **deps:** bump gpui-component from `ff3eb11` to `0e2fb7a` ([d6b7c20](https://github.com/wheregmis/threadlane/commit/d6b7c20c05b74c08b19b6ffa004734622081d1da))
* **deps:** bump gpui-component-assets from `ff3eb11` to `0e2fb7a` ([6c7c54d](https://github.com/wheregmis/threadlane/commit/6c7c54dfa4832619b51e6a95e257388175fc82a2))
* **deps:** bump gpui-component-assets from `ff3eb11` to `0e2fb7a` ([5617345](https://github.com/wheregmis/threadlane/commit/5617345115156f575f514e7107d32d9339de6507))
* **deps:** bump robius-open from `bf2a77f` to `9ca3f2d` ([2b13098](https://github.com/wheregmis/threadlane/commit/2b130983c44f6c552b5fe312d2f06e9459bf065f))
* **deps:** bump robius-open from `bf2a77f` to `9ca3f2d` ([b6595eb](https://github.com/wheregmis/threadlane/commit/b6595eb4512753ef371ae2e0c79d9387e1ce2828))
* **deps:** bump robius-open from `cca3cc3` to `bf2a77f` ([18a13d0](https://github.com/wheregmis/threadlane/commit/18a13d0567f926e549c747c0b7e0cfa953835cec))
* **deps:** bump robius-open from `cca3cc3` to `bf2a77f` ([67a97e0](https://github.com/wheregmis/threadlane/commit/67a97e07352b8b125ff94090d5a1d5c167655ea0))
* Document explicit permission handling ([9c14558](https://github.com/wheregmis/threadlane/commit/9c145584391b69357f92b598b9562fdc5d3affdb))
* gpui and component skills ([e30573d](https://github.com/wheregmis/threadlane/commit/e30573d756e9a5553ae2c10be63f4c599d5bf4ff))

## [0.1.9](https://github.com/wheregmis/threadlane/compare/v0.1.8...v0.1.9) (2026-08-27)


### Features

* **acp:** drop the agent settings control, keep models in the model picker ([975fda8](https://github.com/wheregmis/threadlane/commit/975fda8a3b423256793dca076f0df2ee4302a691))
* **acp:** drop the agent settings control, keep models in the model picker ([5cfab98](https://github.com/wheregmis/threadlane/commit/5cfab985f932b9130e13f0b83bf5491e61149bba))
* **acp:** run turns against external ACP agents end to end ([e04861e](https://github.com/wheregmis/threadlane/commit/e04861ebe28d205e04fc08e425216643699b0cdd))
* **acp:** run turns against external ACP agents end to end ([849c21a](https://github.com/wheregmis/threadlane/commit/849c21aba8d3eaf8b8ced387172176ff96930645))
* Add ACP preset quick setup ([2d932ba](https://github.com/wheregmis/threadlane/commit/2d932bae514caf5e87906594ba70745b9d008ca7))
* Add chat navigation and steering shortcuts ([ef530b1](https://github.com/wheregmis/threadlane/commit/ef530b1c53a5a9990539ef0f51b4b393f3c92310))
* Add chat navigation and steering shortcuts ([b9b47ca](https://github.com/wheregmis/threadlane/commit/b9b47ca01d95e49e2b62f21d8df8001a430978e7))
* **gpui:** Add interactive code block controls ([01bd38d](https://github.com/wheregmis/threadlane/commit/01bd38d826da3e108f159b86ca71665fdcaff03c))
* Persist ACP tool activity in session runs ([1ad503c](https://github.com/wheregmis/threadlane/commit/1ad503cd6748ed5ce5cd4d5e18cb2642ecaf9260))


### Bug Fixes

* address hotpath benchmark review ([d6f55b9](https://github.com/wheregmis/threadlane/commit/d6f55b9d0e8ea38b6d2d7b93b3185885079195a5))
* allow redirects and larger network responses ([ab1cf23](https://github.com/wheregmis/threadlane/commit/ab1cf23ee0fb9babdb5fc304c2dc4114b5fa189a))
* allow redirects and larger network responses ([df3a810](https://github.com/wheregmis/threadlane/commit/df3a81013bce77f542d3b01280acacdee4bc1840))
* Constrain chat transcript width ([c7e0244](https://github.com/wheregmis/threadlane/commit/c7e02443ea1f0acdbb5d86778e7e6d1e326c1cad))
* Correct ACP config and permission handling ([0d33d44](https://github.com/wheregmis/threadlane/commit/0d33d44fcb1ad484032d92494ad207831d7e967c))
* Harden chat and terminal interactions ([20e189e](https://github.com/wheregmis/threadlane/commit/20e189ed8dbca274f0fbed5f9c27494ad66d0a81))
* keep broker redirects behind host approval ([4c2ffe0](https://github.com/wheregmis/threadlane/commit/4c2ffe0e0de4d4dac1a4a299c978bd777c81e94d))
* Preserve queued message attachments ([476aab0](https://github.com/wheregmis/threadlane/commit/476aab03d78747d196ccc2f6432938c154264121))
* Refresh ACP settings and reasoning display ([f3765d7](https://github.com/wheregmis/threadlane/commit/f3765d75e0333d6aa1d41c060d2ecb4f1ff6a5b3))
* refresh session PR data and sidebar state ([ead240b](https://github.com/wheregmis/threadlane/commit/ead240b627c0646174b746cc562ae685f7f1f860))
* Use stable labels for recent timestamps ([091da27](https://github.com/wheregmis/threadlane/commit/091da27455f53b292fcac1563f20eab2d820b7e6))


### Performance Improvements

* add hotpath PR benchmarking ([8387cc2](https://github.com/wheregmis/threadlane/commit/8387cc296cfeb233e2b0830243909ba3293cb502))
* benchmark MCP steady-state paths ([52fe687](https://github.com/wheregmis/threadlane/commit/52fe687e263bab62d8d28b672285481be7171362))
* benchmark terminal parser hot paths ([a18c4ec](https://github.com/wheregmis/threadlane/commit/a18c4ec6dcc6339dbec5a6c33d3f3a2dcf6848a3))
* benchmark warm repository search ([e1f3709](https://github.com/wheregmis/threadlane/commit/e1f37095e9a9be1bdb53e2667e8265adeebf025e))
* Centralize Hotpath benchmarks in workspace crate ([ce43322](https://github.com/wheregmis/threadlane/commit/ce4332259b1276c790b45f885e5a315d496c0f0b))
* expand hotpath PR benchmarks ([cfdcfed](https://github.com/wheregmis/threadlane/commit/cfdcfed2b6d1c5dabb0bbeb96c1538188018bb69))
* expand runtime hotpath benchmark ([e482d97](https://github.com/wheregmis/threadlane/commit/e482d97da40f3f4c10958892e46d2846e9b503b1))
* match terminal benchmark scrollback ([c69e3ff](https://github.com/wheregmis/threadlane/commit/c69e3ffc0aae79fb5e2051d3dc22b9d9ed4ebd54))


### CI

* comment each hotpath benchmark suite ([3a910ac](https://github.com/wheregmis/threadlane/commit/3a910ac32bf72cfb5531fcceb200d330ba614307))
* Expand Hotpath benchmark reporting ([89a8bf4](https://github.com/wheregmis/threadlane/commit/89a8bf444d5994fac999e379fa93fb2fb264232f))
* harden hotpath benchmark comments ([14c6e8e](https://github.com/wheregmis/threadlane/commit/14c6e8e2e7219fa59cc19ba7a8fda1622320a75a))
* preserve hotpath suite paths in artifacts ([9646016](https://github.com/wheregmis/threadlane/commit/96460163c484710a0ffceb7f31d06c2107bf53a9))
* profile deterministic hotpath suites ([1a93b96](https://github.com/wheregmis/threadlane/commit/1a93b962392dce5749a2bc7e79200d40742b89ab))


### Maintenance

* require conventional commit subjects ([8dc24bb](https://github.com/wheregmis/threadlane/commit/8dc24bbf05814a55cdd51a3d18a98fe4d854fd91))
* untrack internal benchmark report ([26204d8](https://github.com/wheregmis/threadlane/commit/26204d8d2d796847d11f49990a4a7ea428c974d0))

## [0.1.8](https://github.com/wheregmis/threadlane/compare/v0.1.7...v0.1.8) (2026-08-26)


### Features

* add commit history inspection and review tab UI ([9889f3b](https://github.com/wheregmis/threadlane/commit/9889f3b4969440671e2ec646ac52d826c430ad98))
* add stash management and file inspection to right panel ([27303fe](https://github.com/wheregmis/threadlane/commit/27303fe1d35873e9e2129365471f832d18dfb75f))
* Advance git operations ([55aca0e](https://github.com/wheregmis/threadlane/commit/55aca0e3d81179d6b24cbbe42ae321845bd720dc))
* Alot of performance nits and fixing subagents ([6af7b7f](https://github.com/wheregmis/threadlane/commit/6af7b7f9fab14331e1d587146ef07d33b60e3486))
* **git:** add branch management and synchronization actions ([7f68f0f](https://github.com/wheregmis/threadlane/commit/7f68f0fe10e79dab5f1af2a20e68180324d91efb))
* **git:** add file discard and ignore actions ([19f36e5](https://github.com/wheregmis/threadlane/commit/19f36e537cedc98b4d8bdd8ca6b4b62648aead48))
* **gpui:** project current context telemetry ([fd93c36](https://github.com/wheregmis/threadlane/commit/fd93c36335c49e37069fe07d292f6eb2030bf789))
* **gpui:** render markdown while streaming ([c3e9009](https://github.com/wheregmis/threadlane/commit/c3e900953a8303440139295a2a9a08fdb1fec666))
* **gpui:** show current model context ([034e2b9](https://github.com/wheregmis/threadlane/commit/034e2b984ae02a0324373a12b38ecc1fbdd7c1ef))
* **harness:** record context compaction telemetry ([2ba4749](https://github.com/wheregmis/threadlane/commit/2ba474975e86e187001ba5764c5484c212687de8))
* multiple terminals, basic selection in terminal ([2f2b1b8](https://github.com/wheregmis/threadlane/commit/2f2b1b8d4a64e565bd7158e8ae40983df5f686e0))
* **runtime:** add adaptive context budgets ([dc18641](https://github.com/wheregmis/threadlane/commit/dc1864114240ce3c671f02e332172082ec635529))
* **runtime:** prepare context before provider attempts ([1b2c094](https://github.com/wheregmis/threadlane/commit/1b2c09496f23654602cc32f1e006e0f74105dff0))
* **runtime:** prepare context to adaptive budgets ([6304a01](https://github.com/wheregmis/threadlane/commit/6304a01c82193bbf778fb717b0521b6dc1f47c58))
* **session:** compact durable context between attempts ([ccba227](https://github.com/wheregmis/threadlane/commit/ccba2270a40b9706596998ae3309c449916ad0dd))


### Bug Fixes

* run subagents on dedicated harness lanes ([4d4c9b9](https://github.com/wheregmis/threadlane/commit/4d4c9b992aa9ad3882274e4543d00bcd6a8c7309))
* **runtime:** align provider boundary tool schema ([116308b](https://github.com/wheregmis/threadlane/commit/116308b0b114b67a6518e87d6b45580741b007c6))
* **runtime:** enforce adaptive compaction budget ([4bbc034](https://github.com/wheregmis/threadlane/commit/4bbc034491c4a7a99000de6a5ef1d19dd722dca0))
* **runtime:** preserve built-in tool failure status ([694fb20](https://github.com/wheregmis/threadlane/commit/694fb20314b81120307dfc492d12132e18a17467))
* stop mouse event propagation in right panel sections ([cae3399](https://github.com/wheregmis/threadlane/commit/cae3399b832ce6d83b98a569055bab21afba17cd))
* **task-4:** restore complete transcript scope ([d6a7193](https://github.com/wheregmis/threadlane/commit/d6a7193bc5c3fcab796930b74dc34b1a50202875))
* **task-5:** align transcript recovery parsing ([9ffb19b](https://github.com/wheregmis/threadlane/commit/9ffb19b88fc031bf09a56bfd734c68e9abe98465))
* **task-5:** close durable recovery proof gaps ([4043310](https://github.com/wheregmis/threadlane/commit/4043310bc3e8e8a5535e106f1ffa79e31e6c07b6))
* **task-5:** constrain torn frame quarantine ([30cd715](https://github.com/wheregmis/threadlane/commit/30cd7150e6a8fca99b7e1442d207b52fb0225ea5))
* **task-5:** harden durable compaction boundaries ([1a8897a](https://github.com/wheregmis/threadlane/commit/1a8897a9754abc41e4dc2fe844367b6528c845f3))
* **task-5:** make compaction append atomically durable ([61fc76c](https://github.com/wheregmis/threadlane/commit/61fc76c6bd3e9bb42076613400b00981968909a5))
* **task-6:** scope durable context projections ([6b631c8](https://github.com/wheregmis/threadlane/commit/6b631c81b9f5137893e2775c56bb6b732bce9cc4))
* **task-7:** make context meter details accessible ([f80242c](https://github.com/wheregmis/threadlane/commit/f80242c4987f38330b977ab75946bbe7710c6431))
* **task-8:** exercise durable tool loop regression ([476f853](https://github.com/wheregmis/threadlane/commit/476f8535be0ef3d41f7f0874cf3ce36cb4c7305d))
* **task-8:** finish durable long-loop integration ([5669622](https://github.com/wheregmis/threadlane/commit/5669622a31e8c12662086fcefa4eb81e824d11eb))
* **tools:** preserve memory on read failure ([59a31e3](https://github.com/wheregmis/threadlane/commit/59a31e3413aafaf843231d490b6c6c43c0bf707c))
* **tools:** propagate directory traversal failures ([7d464c9](https://github.com/wheregmis/threadlane/commit/7d464c900bfa4c0d19b30ba1efdd023e2422974b))
* **tools:** return typed built-in failures ([b707b45](https://github.com/wheregmis/threadlane/commit/b707b45dd52fa503137420b34f754ef22a335c62))
* **tools:** type virtual read failures ([656d4bd](https://github.com/wheregmis/threadlane/commit/656d4bd93ca7ddab0260d54bc886b94d7fd38c50))


### Performance Improvements

* avoid retaining duplicate activity summaries ([546fbf8](https://github.com/wheregmis/threadlane/commit/546fbf8eec408b373cc7933aa8a046c254255b64))
* batch untracked commit diff ([384a014](https://github.com/wheregmis/threadlane/commit/384a014cdaa87f1043fc575040c41fbf689447a3))
* borrow transcript rows during rendering ([c5012c7](https://github.com/wheregmis/threadlane/commit/c5012c7bc19321428ebf0819040f165678aa32cf))
* cache trajectory JSON formatting ([b6115dc](https://github.com/wheregmis/threadlane/commit/b6115dcbd1191523b51c5bec778ab3ba2e065d4d))
* cache workspace roots and bound grep ([c529a47](https://github.com/wheregmis/threadlane/commit/c529a4764546f7a438a9560d68f972ff01a5498f))
* **gpui:** cache provider status and slash command discovery ([82fb859](https://github.com/wheregmis/threadlane/commit/82fb8595a6c3c11b74e365373066eab737718a48))
* **gpui:** update cached markdown incrementally ([3d7b9f1](https://github.com/wheregmis/threadlane/commit/3d7b9f17a10cec756d31d7bfd8815570654db78d))
* isolate owned session filesystem work ([c984a11](https://github.com/wheregmis/threadlane/commit/c984a11d0ab729aba559fbb50cd8f985dc61c11e))
* page transcript JSONL backward ([54ff269](https://github.com/wheregmis/threadlane/commit/54ff269c2cdb4f6d62a091810890d69ca204134e))
* precompute tool activity display summaries ([7f0bfbb](https://github.com/wheregmis/threadlane/commit/7f0bfbb9394c830014f241920cc1b787ce7e238c))
* relax observational journal sync ([e187ce6](https://github.com/wheregmis/threadlane/commit/e187ce680c996bef9ddee68c612b2ca8fe946d84))
* reuse OpenAI client and model list ([cf359a0](https://github.com/wheregmis/threadlane/commit/cf359a0a0db129d862f9e884536241918b725f13))
* share cached tool definitions ([bc93763](https://github.com/wheregmis/threadlane/commit/bc9376398c81ed58e059277232efed4c22e8e835))
* virtualize paged chat history ([eba9559](https://github.com/wheregmis/threadlane/commit/eba95597422f3d27160333d436753f67ccddf077))
* virtualize trajectory events ([7e5591a](https://github.com/wheregmis/threadlane/commit/7e5591ae7137233615c22ef0d95d423ea622b99d))
* wake supervisor on harness events ([e69304f](https://github.com/wheregmis/threadlane/commit/e69304f55c61393c1d13ef8bb3f12e94ef98ecc4))

## [0.1.7](https://github.com/wheregmis/threadlane/compare/v0.1.6...v0.1.7) (2026-08-21)


### Features

* complete revamp of harness and agent ([a31d770](https://github.com/wheregmis/threadlane/commit/a31d77028a70e927c506abb7497d70305079272c))
* **gpui:** add workspace watcher for automatic panel refresh ([1e3288a](https://github.com/wheregmis/threadlane/commit/1e3288a20872c838a4c61fd45a9f7cee7c650ea7))

## [0.1.6](https://github.com/wheregmis/threadlane/compare/v0.1.5...v0.1.6) (2026-08-19)


### Features

* editor ([7a38055](https://github.com/wheregmis/threadlane/commit/7a380555f35b5b93bf9bf3b9905de5e32ca6cbf5))
* editor ([5ecd1fd](https://github.com/wheregmis/threadlane/commit/5ecd1fd750e1057002cb6b75ac53e7fb080645b5))
* resizeable panels ([6849841](https://github.com/wheregmis/threadlane/commit/68498418b1345cd2a3051d7ba1b922fd0852ffb8))

## [0.1.5](https://github.com/wheregmis/threadlane/compare/v0.1.4...v0.1.5) (2026-08-19)


### Features

* add session plans and grouped tool activity display ([f4a646a](https://github.com/wheregmis/threadlane/commit/f4a646ab4eacb41b893e0876db44f5fbeb8c2aba))
* **gpui:** add branch, effort, and command composer controls ([639c8b0](https://github.com/wheregmis/threadlane/commit/639c8b08366d81e77eca2ec2f4c5d75af35a1b23))
* **gpui:** format context-window tooltip token counts readably ([236c90f](https://github.com/wheregmis/threadlane/commit/236c90f70696d7943b29fdee3d9670ccd33ee2ad))
* Roadmap ([223ae03](https://github.com/wheregmis/threadlane/commit/223ae030549b610ed4ac02d815fab3a82fda07aa))
* tracability and trajectory ([75f999c](https://github.com/wheregmis/threadlane/commit/75f999ce24756aa2f9ebe9821a7b41492c60b10e))


### Bug Fixes

* **coding-agent:** correct ${@:-default} prompt-template parsing ([322649d](https://github.com/wheregmis/threadlane/commit/322649dd4b79f609eb3f1b9606b314a015ad1e4f))
* overlay scrollbars and preserve palette selection indexing ([9367618](https://github.com/wheregmis/threadlane/commit/93676186a9b7e38575dfebbcbb54ae3e42f34e65))

## [0.1.4](https://github.com/wheregmis/threadlane/compare/v0.1.3...v0.1.4) (2026-08-15)


### Features

* add canonical session lane facade, event projection, and pending-aware sequences ([e4efa8b](https://github.com/wheregmis/threadlane/commit/e4efa8b79f405feee6bb5519a3f615cf8b2603f2))
* add live harness activity tracking and task resumption ([6e2f278](https://github.com/wheregmis/threadlane/commit/6e2f278a74efe02bad1507cce2b935225f1c685b))
* harness v2 foundation and durable task orchestration ([f6ffe2c](https://github.com/wheregmis/threadlane/commit/f6ffe2c4f2e408284b54a13b69a48c8cbb3f2ea3))


### Bug Fixes

* detect suspended subagents and await stop completion ([537df07](https://github.com/wheregmis/threadlane/commit/537df0754e2fcebf2f26184b456ae3367b2941d8))
* improve diagnostic path matching normalization ([e95f58c](https://github.com/wheregmis/threadlane/commit/e95f58c19e30bcf23601ac7f6b1f9541ab3fb0f3))
* publish artifacts from reusable releases ([b6b9f1c](https://github.com/wheregmis/threadlane/commit/b6b9f1c64be3d18b7b389ba09196384652340891))
* publish artifacts from reusable releases ([47aae24](https://github.com/wheregmis/threadlane/commit/47aae24f16e6214a6f52d3dbaee9b1bdc020fe8e))

## [0.1.3](https://github.com/wheregmis/threadlane/compare/v0.1.2...v0.1.3) (2026-08-06)


### Bug Fixes

* document Release Please commit requirements ([aaf867c](https://github.com/wheregmis/threadlane/commit/aaf867cd2a4cd9713af425efd5c6d12a1978dc9a))
* document Release Please commit requirements ([3dd8d89](https://github.com/wheregmis/threadlane/commit/3dd8d89f259eae846aafff6187f6e71184a08dfc))

## Changelog

All notable changes to Threadlane are recorded here. This file is maintained by
[Release Please](https://github.com/googleapis/release-please) when it prepares a release pull request.
