# Changelog

## [0.1.17](https://github.com/wheregmis/threadlane/compare/v0.1.16...v0.1.17) (2026-09-12)


### Features

* **auth:** Add injectable credential storage ([e483c61](https://github.com/wheregmis/threadlane/commit/e483c615b8906b2394690a0fe1535c5f8e28fc13))


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
* **session:** Simplify module organization ([0a95727](https://github.com/wheregmis/threadlane/commit/0a95727597e10c4c75965eb80bdeff65503c15f7))
* **tools:** Inject remote forge credentials ([b489b8a](https://github.com/wheregmis/threadlane/commit/b489b8a39a32961932b794dc98be4543a2e73e06))


### CI

* skip draft PRs and run checks when ready for review ([c3faf75](https://github.com/wheregmis/threadlane/commit/c3faf75c8d4c8ec5e1bb320984c57549360d2d78))


### Maintenance

* configure nightly Rust with parallel compilation ([ff57a65](https://github.com/wheregmis/threadlane/commit/ff57a650523b515e0c4dad4bfa216326fab74005))
* deduplicate workspace dependencies into root cargo.toml ([445e6bc](https://github.com/wheregmis/threadlane/commit/445e6bcdd139b34d41e745734d3501f4b7cf70c8))
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
