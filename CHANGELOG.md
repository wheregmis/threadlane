# Changelog

## [0.1.59](https://github.com/wheregmis/threadlane/compare/v0.1.58...v0.1.59) (2026-11-24)


### Features

* **hub:** give the lead pr_readiness and a guarded merge ([#763](https://github.com/wheregmis/threadlane/issues/763)) ([12d0505](https://github.com/wheregmis/threadlane/commit/12d050502803d764fdccb7fc5f332c418a77b787))
* **hub:** lead PR readiness read and guarded merge ([#763](https://github.com/wheregmis/threadlane/issues/763)) ([53eb21a](https://github.com/wheregmis/threadlane/commit/53eb21a5155d02ab1c8091ba6831beffdb091d39))


### Bug Fixes

* **claude:** launch agents without the owner's personal Claude setup ([08f8aa9](https://github.com/wheregmis/threadlane/commit/08f8aa9561d0e58680a0bd40a03b9052a2742ca4))
* **desktop:** offer Restart hub when an app-started hub stays behind ([1a545a2](https://github.com/wheregmis/threadlane/commit/1a545a20ab82a2c87cba73dc5f6d3f76163905d0))
* **desktop:** offer Restart hub when an app-started hub stays behind, say when a hub is not the app's to restart ([e259f54](https://github.com/wheregmis/threadlane/commit/e259f54c29f1fdefd378bd6745f94defa94753a2))
* **hub:** count a lead session's context from its last message iteration, withdraw Dreaming history after a restart ([65927a7](https://github.com/wheregmis/threadlane/commit/65927a7c8740194b594c812387d1ab1f95bbc1d1))
* **hub:** count only readable choices so the decision feed's last page ends ([583fe93](https://github.com/wheregmis/threadlane/commit/583fe93168ae174d80b0a7cbe0d12c511fb9ca93))
* **hub:** count only readable choices so the decision feed's last page ends ([#757](https://github.com/wheregmis/threadlane/issues/757)) ([f627df4](https://github.com/wheregmis/threadlane/commit/f627df496d74db97bd00480d8bb74754c45a04b7))
* **hub:** follow-ups to [#758](https://github.com/wheregmis/threadlane/issues/758) — lead context from last message iteration, withdraw history after restart ([392c0c4](https://github.com/wheregmis/threadlane/commit/392c0c460129096128f1a89904e369f932e7a3db))
* **hub:** stop the lead rereading its preference history on every wake ([e0e53c4](https://github.com/wheregmis/threadlane/commit/e0e53c4fa2b41d7e7e5e3f94101469a3134f790e))

## [0.1.58](https://github.com/wheregmis/threadlane/compare/v0.1.57...v0.1.58) (2026-11-24)


### Features

* add create_tag to tag a verified commit without publishing a release ([#749](https://github.com/wheregmis/threadlane/issues/749)) ([8e96a5d](https://github.com/wheregmis/threadlane/commit/8e96a5db82a47f1ebb680136a12a4f9470d012b8))
* link PRs handled by a batch merge or mark_ready to the task's work ([#747](https://github.com/wheregmis/threadlane/issues/747)) ([8e96a5d](https://github.com/wheregmis/threadlane/commit/8e96a5db82a47f1ebb680136a12a4f9470d012b8))
* show why failed and abandoned runs ended in session review ([#751](https://github.com/wheregmis/threadlane/issues/751)) ([8e96a5d](https://github.com/wheregmis/threadlane/commit/8e96a5db82a47f1ebb680136a12a4f9470d012b8))


### Bug Fixes

* **hub:** name valid tool fields, cool usage limits 30 min, drop undeliverable reviews, mute mDNS log ([724b03e](https://github.com/wheregmis/threadlane/commit/724b03eb346dfabec70f60554b1c906fc9e29a2a))
* **hub:** tell a re-review what its earlier rounds asked for ([ad71558](https://github.com/wheregmis/threadlane/commit/ad7155848acfa39f4deafd57441a578bc1d0b3a8))
* keep the app's local hubs on the app's build and refuse a stale hub program before stopping a hub ([#752](https://github.com/wheregmis/threadlane/issues/752)) ([8e96a5d](https://github.com/wheregmis/threadlane/commit/8e96a5db82a47f1ebb680136a12a4f9470d012b8))

## [0.1.57](https://github.com/wheregmis/threadlane/compare/v0.1.56...v0.1.57) (2026-11-23)


### Features

* **client:** add decisions stats ([b63e691](https://github.com/wheregmis/threadlane/commit/b63e6914b427dfc0df12861a64bc547ad9bf3e6f))
* decline review threads with a reply, and diff PR reviews against the PR's base ([0479807](https://github.com/wheregmis/threadlane/commit/0479807006f0499b3f480d89850839bc6d26deba))
* **desktop,hub:** live execution feed, quick-create and direct board ([d1a6361](https://github.com/wheregmis/threadlane/commit/d1a6361203985c42b0f3b3962e9c728d2cf494fd))
* **desktop:** [@agent](https://github.com/agent) dispatch and #task picker in the composer ([a3fe3a3](https://github.com/wheregmis/threadlane/commit/a3fe3a3a571456035c5cfbe34bca9c0ba8a67b29))
* **desktop:** [@agent](https://github.com/agent) dispatch and #task picker in the composer ([681e352](https://github.com/wheregmis/threadlane/commit/681e352272c8b554864d893f22fc55261382e845))
* **desktop:** PR/CI strip and full-width in-place review ([72e6a03](https://github.com/wheregmis/threadlane/commit/72e6a03bc517bcb1fde431739bd602db497e3e7c))
* **gate:** run read-only github_cli checks without an approval card ([c734426](https://github.com/wheregmis/threadlane/commit/c7344267ac8f2b9c1773b0d1d91d1b6e1a338e86))
* **gate:** run read-only github_cli checks without an approval card ([0b58ba8](https://github.com/wheregmis/threadlane/commit/0b58ba8e3f08a2863883db4fd296af7874a78d02)), closes [#706](https://github.com/wheregmis/threadlane/issues/706)
* give each project one mode that its agents follow ([ded087c](https://github.com/wheregmis/threadlane/commit/ded087c850ec54fd134daaebc1f3b25676920772))
* **hub,runner:** attach_image forge tool posts a local screenshot on a PR ([239a333](https://github.com/wheregmis/threadlane/commit/239a3337a2cf731487d889f3fc6a1a7855ab912c))
* **hub,runner:** attach_image forge tool posts a local screenshot on a PR ([#735](https://github.com/wheregmis/threadlane/issues/735)) ([82ad102](https://github.com/wheregmis/threadlane/commit/82ad102d037eb47e073a6c5e3fd518bfdeb23d32))
* **hub:** count another requested task's pull request as own work ([d3f34f2](https://github.com/wheregmis/threadlane/commit/d3f34f20f0b7ee672bf0e0a02ecd985dc53b67d8))
* **hub:** finish_review checks partial / not_applicable with a required note ([a625ee6](https://github.com/wheregmis/threadlane/commit/a625ee6eadbb4f0a354dbd0d8395eb5a1faa7928))
* **hub:** finish_review checks partial / not_applicable with a required note ([c089553](https://github.com/wheregmis/threadlane/commit/c089553d83c9a2088b691d0d19740e1fad316776)), closes [#690](https://github.com/wheregmis/threadlane/issues/690)
* **hub:** let a requested task's own steps through the gate ([ab06864](https://github.com/wheregmis/threadlane/commit/ab068645165b9c851fa17ede9a95d3955b3d2432))
* **hub:** let task runs list tasks and the lead view issues and PRs ([e2cbf00](https://github.com/wheregmis/threadlane/commit/e2cbf002aa1b1fc23a63d1add80866c15238decc)), closes [#712](https://github.com/wheregmis/threadlane/issues/712) [#714](https://github.com/wheregmis/threadlane/issues/714)
* **hub:** live pull request and CI status for tasks ([f2f2358](https://github.com/wheregmis/threadlane/commit/f2f23583345d85c8745e6827f0cd64a0f6a57bad))
* **hub:** make standing rules cover the whole project ([30743f2](https://github.com/wheregmis/threadlane/commit/30743f2e2b01952fafddba7bb1e06c9af8453393))
* **hub:** review a tainted run's own draft instead of asking ([9a8aa8c](https://github.com/wheregmis/threadlane/commit/9a8aa8c781bad5e47d8c68894546f1d239df0a0a))
* **hub:** task runs list tasks; lead views issues and PRs read-only ([6f72517](https://github.com/wheregmis/threadlane/commit/6f72517d25512ad867e1327ae07f1b09d0b67d0c))
* one-line installer and README refresh ([666d711](https://github.com/wheregmis/threadlane/commit/666d7112ed87680a890313b3e351e88cb361a0c1))
* send proposals and limitation reports to a Later list ([1aba834](https://github.com/wheregmis/threadlane/commit/1aba834943da415dfec7ad14f923d91d43606d71))
* **skills:** add composable space skills library attachable to agents and tasks ([2377090](https://github.com/wheregmis/threadlane/commit/2377090b75aecfb17b5f885da668076bd5218a11))
* **store,hub:** add tiered task compaction and pruning engine ([de88c1b](https://github.com/wheregmis/threadlane/commit/de88c1b0320846c3ca59820bb8862ea376ae71c1))


### Bug Fixes

* **desktop,hub:** address [#729](https://github.com/wheregmis/threadlane/issues/729) review findings ([4fae587](https://github.com/wheregmis/threadlane/commit/4fae587eb46ff76ca2bb868768ebede1fdc13374))
* **desktop:** dispatch only picked @ chips, block images, retry only unstarted agents ([5c1a715](https://github.com/wheregmis/threadlane/commit/5c1a71530131db7af0db1e4df062e817187c7518))
* **desktop:** dispatch only picked @ chips, block images, retry only unstarted agents ([ae074ba](https://github.com/wheregmis/threadlane/commit/ae074ba7b830b2625b0a81360e8559bf91a0f2db))
* **desktop:** tie [@agent](https://github.com/agent) picks to their chip and reuse each dispatch's request key ([70584d6](https://github.com/wheregmis/threadlane/commit/70584d6e6a15fda54a4c0d777f59c4212cf87574))
* don't point PR reviews at origin/&lt;base&gt; when fetching it failed ([fd96e3e](https://github.com/wheregmis/threadlane/commit/fd96e3e323b0fa09709c2acca546cc6b4b2dda6b))
* **hub:** attach images a lead message names by path ([#733](https://github.com/wheregmis/threadlane/issues/733)) ([c88321b](https://github.com/wheregmis/threadlane/commit/c88321b9d13e6ba2014b8ff7dbfc4ec7faa44ebf))
* **hub:** budget, validate and batch lead path images ([#733](https://github.com/wheregmis/threadlane/issues/733)) ([49daa2a](https://github.com/wheregmis/threadlane/commit/49daa2a33b7f19cc4d2fc3d8e293d209bba84c8f))
* **hub:** keep saving progress while an approval waits ([b307492](https://github.com/wheregmis/threadlane/commit/b307492a7bfa90dbfdce113ea7f0be0c36a02287))
* **hub:** keep saving progress while an approval waits ([36eea95](https://github.com/wheregmis/threadlane/commit/36eea955acf3e2376d0c4ab72083db3703c2d875)), closes [#736](https://github.com/wheregmis/threadlane/issues/736)
* **hub:** let Always allow also answer matching cards already waiting ([cc6731f](https://github.com/wheregmis/threadlane/commit/cc6731fb9fe63bf62be9ddd34d3e1cc910304bb7))
* **hub:** let the lead see images a message names by path ([7473244](https://github.com/wheregmis/threadlane/commit/7473244e696a5bd29a7652ef1f9346f038be0300))
* **hub:** list project_sessions tasks compactly so a page holds 50+ ([e7b04bf](https://github.com/wheregmis/threadlane/commit/e7b04bf52550d9ea333687c9b9cec70a6215fad8))
* **hub:** pin own-work merges to the approved head and read every check run ([5d31b68](https://github.com/wheregmis/threadlane/commit/5d31b689a8fb52dd92f06200415f22b440445abd))
* **hub:** pin own-work merges to the approved head and refuse partial check pages ([cd093f1](https://github.com/wheregmis/threadlane/commit/cd093f14b97c77d8aad0e794b72c8d6ee921781e))
* **hub:** pr_review_status accepts owner/repo and PR URLs ([#704](https://github.com/wheregmis/threadlane/issues/704)) ([3f332a7](https://github.com/wheregmis/threadlane/commit/3f332a7d8097d360c16bcab4400c7a3342b274e3))
* **hub:** remove an ended task's worktrees once nothing in them would be lost ([b0ab879](https://github.com/wheregmis/threadlane/commit/b0ab87985a8e3ecfb46392495831d2829332fe7e))
* **hub:** resolve owner/repo and PR URLs in pr_review targets ([#704](https://github.com/wheregmis/threadlane/issues/704)) ([07764ae](https://github.com/wheregmis/threadlane/commit/07764ae3eccba54c574115bedbae32300bd37131))
* **hub:** retry an approved merge refused only because the base moved ([9d7cd8e](https://github.com/wheregmis/threadlane/commit/9d7cd8e8257f9abac6d820c3174d25d960dee1b7))
* **hub:** run ungated calls made after a gated call in the same turn ([96ea105](https://github.com/wheregmis/threadlane/commit/96ea10512ed577c22e34452ade7afe5ede6b9391))
* **hub:** run ungated calls made after a gated call in the same turn ([66ecbb3](https://github.com/wheregmis/threadlane/commit/66ecbb36bef0e75061ddb86eaa3d3777c536fc5a)), closes [#718](https://github.com/wheregmis/threadlane/issues/718)
* **memory:** stop preloading and auto-indexing task progress files ([72e3fef](https://github.com/wheregmis/threadlane/commit/72e3fef235c661bca8d22d5c4b32245ff805661b))
* refuse runs of ended tasks and see changes in ignore=all submodules ([6a62fd1](https://github.com/wheregmis/threadlane/commit/6a62fd1ff05e1dc2f4828553208673c082d2f995))
* **runner,hub:** merge main into shared task branches instead of rebasing ([#730](https://github.com/wheregmis/threadlane/issues/730)) ([59efc5c](https://github.com/wheregmis/threadlane/commit/59efc5c6b99b6d5ef0f265b54464fec62c7b5da4))
* **runner:** keep untracked files out of a timed-out run's commit ([8e3ccbe](https://github.com/wheregmis/threadlane/commit/8e3ccbe448ab0cbc62effc1f767ee5b93c192336))
* **runner:** keep untracked files out of a timed-out run's commit ([6a011e8](https://github.com/wheregmis/threadlane/commit/6a011e82138e9b06e859fb8546f49c014c34411c)), closes [#691](https://github.com/wheregmis/threadlane/issues/691)
* **runner:** keep worktrees with hidden untracked or stray dot files ([6dc3a8e](https://github.com/wheregmis/threadlane/commit/6dc3a8e3030d333824e53766b6e86d3098760d90))
* **runner:** merge stacked PRs through the async merge REST API ([5b106ed](https://github.com/wheregmis/threadlane/commit/5b106ed946e61c72a208dc06d8ee4112e09dd944))
* **runner:** merge stacked PRs through the async merge REST API ([#740](https://github.com/wheregmis/threadlane/issues/740)) ([c0c15aa](https://github.com/wheregmis/threadlane/commit/c0c15aa5d603461ca74926bcbad23455fb42cdcd))
* **runner:** parse the final merge poll and refuse a batch's async merge over open downstack PRs ([cabc2e5](https://github.com/wheregmis/threadlane/commit/cabc2e598432b573e60ec12c68d968f8c3264663))
* **runner:** tell the resumed run what an unfinished checkpoint left out ([7f63cee](https://github.com/wheregmis/threadlane/commit/7f63ceea556765db07323bc5627198d4ab642a51)), closes [#691](https://github.com/wheregmis/threadlane/issues/691)
* **skills:** address CodeRabbit findings on [#732](https://github.com/wheregmis/threadlane/issues/732) ([1d68883](https://github.com/wheregmis/threadlane/commit/1d68883e2e4ce43d016ac7a4f160ea6a9e1c6ac3))
* **skills:** make task mentions atomic and ignore stale skill loads ([6321bc9](https://github.com/wheregmis/threadlane/commit/6321bc9e3a03d7e206f157cd80f0a63bc9dce158))
* **store:** leave cancelled decisions out of stats, no rule hint for questions ([60c519f](https://github.com/wheregmis/threadlane/commit/60c519f2d65f9507218d6db3aeefae7fa728832b))

## [0.1.56](https://github.com/wheregmis/threadlane/compare/v0.1.55...v0.1.56) (2026-11-21)


### Bug Fixes

* **hub:** give review runs the task lane's memory tools ([#623](https://github.com/wheregmis/threadlane/issues/623)) ([c657f0d](https://github.com/wheregmis/threadlane/commit/c657f0dcb7ad088a18dd41238ff9c235fb6822fd))
* **hub:** offer pr_review_status to review runs ([#682](https://github.com/wheregmis/threadlane/issues/682)) ([2cca5c8](https://github.com/wheregmis/threadlane/commit/2cca5c82ee4f058fb2fb2740357ee8f01474dba1))
* **hub:** omit diff patches and repeated review feedback from listed session tasks ([a4a335d](https://github.com/wheregmis/threadlane/commit/a4a335d450837507962c3926c81e1df699e5191a))
* **hub:** retry a limitation report that failed to file ([7bbd258](https://github.com/wheregmis/threadlane/commit/7bbd2583657679e6f33883962d1437764c0d598f))

## [0.1.55](https://github.com/wheregmis/threadlane/compare/v0.1.54...v0.1.55) (2026-11-21)


### Features

* add routine history, project outputs and reusable review workflows ([7036450](https://github.com/wheregmis/threadlane/commit/70364505500fa37552501040db1844b9ffc2ec62))
* agent skill bindings (replaces [#561](https://github.com/wheregmis/threadlane/issues/561), merged with current main) ([1e40623](https://github.com/wheregmis/threadlane/commit/1e406237605f7463f53712154ef63f2e984a7476))
* **antigravity:** support plugin skills in agent profiles ([91778bb](https://github.com/wheregmis/threadlane/commit/91778bb1bc966c9b9e50929ac2f03f4cdacc794c))
* **desktop,client:** Inbox running tasks, model picker, space defaults, folder-first repos (supersedes [#560](https://github.com/wheregmis/threadlane/issues/560), [#562](https://github.com/wheregmis/threadlane/issues/562)) ([8468e4a](https://github.com/wheregmis/threadlane/commit/8468e4a57640435a3cd4fbec2a49e94f1116ebd2))
* **desktop:** add folder-first repository setup (T-0031) ([d5d1187](https://github.com/wheregmis/threadlane/commit/d5d11874e22cbf9857dbb34e77ead551eac379bf))
* **desktop:** elevate Chat panel with speaker cards, lead badges, and collapsible handoffs ([8e26802](https://github.com/wheregmis/threadlane/commit/8e26802450a2de29e318b3327bff6ee6cdba0074))
* **desktop:** Give review workflows a project tab ([096d889](https://github.com/wheregmis/threadlane/commit/096d889de829e81f6d4cfa8c3176b2f1498e69fe))
* **desktop:** Inspect historical agent memory versions ([e0748df](https://github.com/wheregmis/threadlane/commit/e0748df6a58a6e7aaddf16b71e5d1231918be956))
* **desktop:** interactive task dependency DAG and live agent topology canvas ([5ecf3d2](https://github.com/wheregmis/threadlane/commit/5ecf3d2a49f9c7f4e812ca218cf3f0d25b33d644))
* **desktop:** link #N references in the Inbox ([0333895](https://github.com/wheregmis/threadlane/commit/03338959e7a8c431ac18ec6b1f0767dd742adba2))
* **desktop:** link #N references in the Inbox ([3ef53a9](https://github.com/wheregmis/threadlane/commit/3ef53a9e98e2254887680666994fc387f206bce2))
* **desktop:** redesign Inbox triage and composer ([53723b4](https://github.com/wheregmis/threadlane/commit/53723b423e7d1ecfb340d4f3fd44b2cd02baf242))
* **desktop:** redesign Machines and Space usage ([1999cbb](https://github.com/wheregmis/threadlane/commit/1999cbb9285a0be76fbc53a734bf8412761c5093))
* **desktop:** redesign Team and Routines ([c28575c](https://github.com/wheregmis/threadlane/commit/c28575c41796eb9b31dc638594592b9a21fcb110))
* **desktop:** show each agent's backends and models on Team cards ([ff73456](https://github.com/wheregmis/threadlane/commit/ff73456c50bfbb00531d132f3e4f3b69c0971452)), closes [#629](https://github.com/wheregmis/threadlane/issues/629)
* **desktop:** Show outbox delivery health in space settings ([d7886bd](https://github.com/wheregmis/threadlane/commit/d7886bd71296857b95bb3a389d10789f4299fd5f))
* **desktop:** show provider-specific model choices in agent settings (T-0030) ([e0d5d28](https://github.com/wheregmis/threadlane/commit/e0d5d28736e907603426ed2a1f039c5445db7165))
* **desktop:** show running tasks in the Inbox (T-0027) ([e8e09ff](https://github.com/wheregmis/threadlane/commit/e8e09ff077b25e1622119a7279f99a040ddfc4ec))
* **desktop:** Show running tasks on the Inbox's Needs you tab ([de9e285](https://github.com/wheregmis/threadlane/commit/de9e285a923a780dc82aaa4f68d08c027fd5565a))
* **desktop:** start the hub of a space the app creates; hide General on Team ([0c8f34e](https://github.com/wheregmis/threadlane/commit/0c8f34e30f862cf29da37b849e52b7a91aaa34a2))
* **desktop:** suggest a default directory for new spaces (T-0033, T-0029) ([e4cd984](https://github.com/wheregmis/threadlane/commit/e4cd98446fcf827d460fe8b74e43e0acde1a890f))
* **desktop:** surface machine activity and redesign the composer ([b3c308c](https://github.com/wheregmis/threadlane/commit/b3c308ca5c587848a5e5e07b3c425f194f136178))
* **desktop:** tier 1 UI/UX triage, diff previews, and metadata formatting ([9efe1c2](https://github.com/wheregmis/threadlane/commit/9efe1c26a6240ffbd8962002537e0c95e211dcb6))
* **desktop:** Tooltips on the project tab bar's icon-only tabs ([6c63eb8](https://github.com/wheregmis/threadlane/commit/6c63eb8377ec67a43bd8c01702d8442f357b5ce0))
* **desktop:** UI/UX enhancements for navigation, shell, design tokens, and screen polish ([eef52f4](https://github.com/wheregmis/threadlane/commit/eef52f41a55272884257deb471df65f8ea99c01b))
* **dreaming:** learn project preferences and resume memory reviews ([929c44b](https://github.com/wheregmis/threadlane/commit/929c44bd7fa27a264785efb42950fac7f8e7c2d8))
* **dreaming:** structured task records and incremental session review ([852a06a](https://github.com/wheregmis/threadlane/commit/852a06a567f73ecd625eba852693a74a29600161))
* **forge:** add approval-gated label_issue tool ([885ed2d](https://github.com/wheregmis/threadlane/commit/885ed2d7f01417c2e6fade096a894968561ccdbf))
* **forge:** approval-gated label_issue tool for issue triage ([d57df46](https://github.com/wheregmis/threadlane/commit/d57df464b49d12c8f83d06dfc9466f175877ca0c))
* **forge:** assign_issue claims an issue before work starts ([c2356e2](https://github.com/wheregmis/threadlane/commit/c2356e27efae1b18dc19977191f20ff2f087d8b9))
* **gate:** open draft pull requests and push the task branch without asking ([ee7a9f0](https://github.com/wheregmis/threadlane/commit/ee7a9f052914e59a5c258656b903a9bf8cc82bf2))
* **hub:** ask before taking an issue someone else holds ([310b4b5](https://github.com/wheregmis/threadlane/commit/310b4b574a3502b3e19e0efe012be387de1f3bab))
* **hub:** ask the user before a gh fallback instead of stopping ([54461d7](https://github.com/wheregmis/threadlane/commit/54461d76136300bbca6e0583929b556e4bb3a7ca))
* **hub:** ask the user before a gh fallback instead of stopping ([77446aa](https://github.com/wheregmis/threadlane/commit/77446aa0ff31026b787c0c64201207acdef1b294))
* **hub:** Close a superseded pull request and delete its branch ([#613](https://github.com/wheregmis/threadlane/issues/613)) ([c2f6680](https://github.com/wheregmis/threadlane/commit/c2f6680082ac226b61358857ac881bfdad4e052c))
* **hub:** Edit an existing pull request's title and body ([#637](https://github.com/wheregmis/threadlane/issues/637), [#643](https://github.com/wheregmis/threadlane/issues/643)) ([38b8cc9](https://github.com/wheregmis/threadlane/commit/38b8cc992b24cc23d2a1949882a178218e8a8d4d))
* **hub:** learn lead preferences from Dreaming ([9bd8db1](https://github.com/wheregmis/threadlane/commit/9bd8db11a61e10a683a23062ae082419a8a4c11c))
* **hub:** let the lead retry a review that failed on configuration ([bfc376f](https://github.com/wheregmis/threadlane/commit/bfc376f8e2d88b10e70bd685a7e263f1ac4e91f1)), closes [#672](https://github.com/wheregmis/threadlane/issues/672)
* **hub:** Name an optional base branch when opening a task's pull request ([#636](https://github.com/wheregmis/threadlane/issues/636)) ([72096aa](https://github.com/wheregmis/threadlane/commit/72096aab3b0c684baa5f79303f3511b9cdd7deab))
* **hub:** Optional base branch for task pull requests ([54277a2](https://github.com/wheregmis/threadlane/commit/54277a246cb57838218886d7601f8c68abc0287b))
* **hub:** report Threadlane limitations from lead runs ([1894322](https://github.com/wheregmis/threadlane/commit/189432232054b843e0cc8b1e1274fc44b3b2b05a))
* **hub:** Resolve a review thread and request a fresh review ([#624](https://github.com/wheregmis/threadlane/issues/624)) ([ab0fb36](https://github.com/wheregmis/threadlane/commit/ab0fb36f6e1a053e9b662f3fab27f6077086956e))
* **hub:** review by default, follow finished pull requests, clean up worktrees ([45fdc38](https://github.com/wheregmis/threadlane/commit/45fdc3838439cc81dabec7beee63a727ae42e5c5))
* **hub:** show the hub's build commit and start time to every run ([ce03048](https://github.com/wheregmis/threadlane/commit/ce03048b608173b381faccd05542e09c506df81f))
* **hub:** show the hub's build commit and start time to every run ([f20fb06](https://github.com/wheregmis/threadlane/commit/f20fb067c53868fa3fa0d4c3a8d7f3d25c41d579)), closes [#652](https://github.com/wheregmis/threadlane/issues/652)
* remove project workflows ([9918835](https://github.com/wheregmis/threadlane/commit/991883554631cdcc62b0b81a2861587dd24e9cb6))
* restore in-app desktop and hub updates ([45caf91](https://github.com/wheregmis/threadlane/commit/45caf917c7f95e9e4cf455ec1e87c7e7c4dea831))
* **routines:** Skip overdue runs outside an owner-selected grace ([00d580b](https://github.com/wheregmis/threadlane/commit/00d580b574fe392c558d5480d664895b4a76300a))
* show publication eligibility and attach repositories to a task ([#616](https://github.com/wheregmis/threadlane/issues/616)) ([0a13cdf](https://github.com/wheregmis/threadlane/commit/0a13cdffa0c1ad144321c05d11708e0baf5fd8a5))
* **tasks:** keep a missing-repository blocker and attach without waking ([#625](https://github.com/wheregmis/threadlane/issues/625)) ([5358a29](https://github.com/wheregmis/threadlane/commit/5358a298de22c55022b46ea13a30f26136b11091))
* **tasks:** open the draft pull request early as the progress record ([4a48797](https://github.com/wheregmis/threadlane/commit/4a487979461f7b13cc307b94120618098716a25c))


### Bug Fixes

* **antigravity:** Cool a quota-limited account down until agy's reset time ([94023d3](https://github.com/wheregmis/threadlane/commit/94023d3a99f427483349e43e79904e37ab1c3703))
* **ci:** Release the hub directory lock on close ([6c3a935](https://github.com/wheregmis/threadlane/commit/6c3a9356546bf35b318eee2491b94c152c0da50d))
* **core:** a skill list that cannot be read skips bound skills instead of failing the run ([e11c003](https://github.com/wheregmis/threadlane/commit/e11c003fe9bb6b1002f027fa67b93bb9ce1e71e3))
* **core:** deny every gh subcommand that changes GitHub ([7a61d28](https://github.com/wheregmis/threadlane/commit/7a61d2868aa60102c5e569ed110ce723e5b4a927))
* **core:** skip an unresolvable bound skill with a warning instead of failing the run ([a80dd83](https://github.com/wheregmis/threadlane/commit/a80dd839c1fed53f96d2493abaeecbd21c175591))
* **desktop,client:** address [#560](https://github.com/wheregmis/threadlane/issues/560) review findings (T-0092) ([31181ae](https://github.com/wheregmis/threadlane/commit/31181ae14fc3a7dd5175dc63f2f5baae82ccf00e))
* **desktop,client:** address [#562](https://github.com/wheregmis/threadlane/issues/562) review findings (T-0108) ([782c543](https://github.com/wheregmis/threadlane/commit/782c543e4d6098c57203e9334eb6c4cabbd83613))
* **desktop:** add skills to Agent literals from [#556](https://github.com/wheregmis/threadlane/issues/556) topology tests ([7e74781](https://github.com/wheregmis/threadlane/commit/7e74781cc5ae41b8737d8b0b90ff860a7e3e3f2c))
* **desktop:** address PR [#556](https://github.com/wheregmis/threadlane/issues/556) review findings for DAG, Topology, and accessibility ([0a598f4](https://github.com/wheregmis/threadlane/commit/0a598f49033da74a124a77579ed83e93b01e963d))
* **desktop:** Coalesce overlapping refreshes ([116d4c0](https://github.com/wheregmis/threadlane/commit/116d4c04d44bb6102249eec2ec16daebedf4ec5f))
* **desktop:** declutter Inbox by folding quiet updates, capping sections, and tightening headers ([4977419](https://github.com/wheregmis/threadlane/commit/4977419ceec6b0512ca20a64e9b651fc2024cc99))
* **desktop:** enable 2D scrolling and top-align connected roots in graph views ([b772244](https://github.com/wheregmis/threadlane/commit/b772244eefca1827623e036b1ff74aff72dd2f28))
* **desktop:** Erase the focusable task form element type ([04f74c1](https://github.com/wheregmis/threadlane/commit/04f74c16612de403a8ab0bb1f59b1599ccbd88c7))
* **desktop:** Expose overflowing project tabs and guard deferred picker changes ([77eee3f](https://github.com/wheregmis/threadlane/commit/77eee3f23f64fc14bbd1ff02b94d8826ba8015d7))
* **desktop:** Focus the visible project composer from Tasks ([48aa551](https://github.com/wheregmis/threadlane/commit/48aa551d9d7bc2d7aeb78a92d54c0d6716225436))
* **desktop:** keep an empty task repository set empty ([f7e9d3b](https://github.com/wheregmis/threadlane/commit/f7e9d3be5df005ae73f5cfb7ff50b14f0b0a7e6b))
* **desktop:** keep approval card titles literal ([351fcc3](https://github.com/wheregmis/threadlane/commit/351fcc3392ad59a77c2a181c78381097a2e84aeb))
* **desktop:** keep approval detail lines literal ([f57f708](https://github.com/wheregmis/threadlane/commit/f57f708b58929b7177d6d7d1132392a618680ee5))
* **desktop:** keep chat scrollbars clear of content ([#574](https://github.com/wheregmis/threadlane/issues/574)) ([79bfd8e](https://github.com/wheregmis/threadlane/commit/79bfd8eb9ac41686cd2ca61b91a7a68a7d917fa5))
* **desktop:** Label the New Task form and restore focus ([8d17c89](https://github.com/wheregmis/threadlane/commit/8d17c890edfa4241fbbeba57b3857344cb821e06))
* **desktop:** leave backslash-escaped \#N references plain ([5878a0f](https://github.com/wheregmis/threadlane/commit/5878a0f73c179fbe0a1af5e05f29222d56a15898))
* **desktop:** leave backslash-escaped \#N references plain ([4c559f3](https://github.com/wheregmis/threadlane/commit/4c559f3c79edc47d1be6fa70e2967fa6fa884458))
* **desktop:** link #N only to an unambiguous repository ([0941bef](https://github.com/wheregmis/threadlane/commit/0941bef1a0821e4e9ccac858412a1f079c94c35f))
* **desktop:** Make chat history loading retryable ([1ebd7da](https://github.com/wheregmis/threadlane/commit/1ebd7da6d2906ef231e7d9c36b3f51a94b081fe0))
* **desktop:** Make the project chat rail responsive ([1a261d0](https://github.com/wheregmis/threadlane/commit/1a261d027e61b0d191f5d253db2e5f6d0d77e28b))
* **desktop:** Own paired hub rows in refresh futures ([3c207d0](https://github.com/wheregmis/threadlane/commit/3c207d0450808732d98fc4d00863b44321b447f0))
* **desktop:** Pass mutable app context when restoring focus ([17fd6c4](https://github.com/wheregmis/threadlane/commit/17fd6c49a3f00e91f7e83fba0994c5b3cdb99854))
* **desktop:** Preserve agent drafts across saves ([93687bb](https://github.com/wheregmis/threadlane/commit/93687bb29eb941880f232f889e4962525a9092d5))
* **desktop:** preserve explicit markdown links and validate ASCII in ISO timestamp parser ([946e9d4](https://github.com/wheregmis/threadlane/commit/946e9d4701ad28fac0c830a4ffbfc475cbcc2c0c))
* **desktop:** Reflow Routines, Team, and Review Workflow ([624a6ac](https://github.com/wheregmis/threadlane/commit/624a6ac9dff9a20db12ec1875b7457f702477290))
* **desktop:** Reload the Inbox timeline after a full refresh pass ([eb7227e](https://github.com/wheregmis/threadlane/commit/eb7227eb4258a192f455a2167b4442b6ce67026e))
* **desktop:** Team topology graph fills the available height ([9b899ed](https://github.com/wheregmis/threadlane/commit/9b899edff543dc80f722e709b61c3b6a45a0306b))
* **dreaming:** page answered choices newest first with no cap ([9671d3b](https://github.com/wheregmis/threadlane/commit/9671d3baca84e1c9ef38ff68cf8d7e77c3b83e5a))
* **dreaming:** page answered choices newest first with no cap ([dcf7969](https://github.com/wheregmis/threadlane/commit/dcf79697a672559e196d897c682eea4a6e3aed07)), closes [#621](https://github.com/wheregmis/threadlane/issues/621)
* **dreaming:** read task activity times before the task rows ([8a421ff](https://github.com/wheregmis/threadlane/commit/8a421ffc0f1f7a5015234f3b9b7742283fb9f13c))
* **dreaming:** suspend defaults while older evidence is unread ([4d860bd](https://github.com/wheregmis/threadlane/commit/4d860bd30b2ad164aeb63d518e974582a095ca77))
* **gate:** record a default-allowed answer as the system's, not the user's ([1a997a6](https://github.com/wheregmis/threadlane/commit/1a997a6b7dd5376f4c863611185188b5d2a58155))
* **hub,runner:** post task reviews on the pull request the task updated ([0f0b8cf](https://github.com/wheregmis/threadlane/commit/0f0b8cf3679c1d3f32fa2d14f0ba4226bac3ad2c))
* **hub,runner:** update a pull request in place instead of superseding it ([c00824d](https://github.com/wheregmis/threadlane/commit/c00824dcd0c46d03fd1a7a8da390e7cc6fc34904))
* **hub:** address review comments and format workspace for CI ([494a62f](https://github.com/wheregmis/threadlane/commit/494a62f1d15c5dfbfd42e4b7f6102f0e5a4da270))
* **hub:** answer parallel approval-gated calls without stopping the run ([8b38fd3](https://github.com/wheregmis/threadlane/commit/8b38fd3f0b1ec77d93bc245ad1eac2a449468311))
* **hub:** answer parallel approval-gated calls without stopping the run ([ef8e0d3](https://github.com/wheregmis/threadlane/commit/ef8e0d3dd237b6ea950de6d2d67196504ad214fa)), closes [#675](https://github.com/wheregmis/threadlane/issues/675)
* **hub:** count task-review approvals under every registration of a remote ([95c94cd](https://github.com/wheregmis/threadlane/commit/95c94cd8a946b36c7a5fdb37d28ad91b1353d09b))
* **hub:** Deliver a lead's reply to the owner's answer; keep the last valid stay_silent ([#604](https://github.com/wheregmis/threadlane/issues/604)) ([05189aa](https://github.com/wheregmis/threadlane/commit/05189aadb2ac4b017e16c572fbeed06e5a11c38f))
* **hub:** Give lead runs an explicit stay_silent outcome ([#604](https://github.com/wheregmis/threadlane/issues/604)) ([e45fb2b](https://github.com/wheregmis/threadlane/commit/e45fb2beaf1c1800d3e399b0cb83003d86fecefa))
* **hub:** Keep a Mac awake while the hub runs ([13bb5b9](https://github.com/wheregmis/threadlane/commit/13bb5b91475a7a6d4b7d163a8a1133b971c16ccb))
* **hub:** Keep a waiting run's final text and runner stop notice out of chat ([#612](https://github.com/wheregmis/threadlane/issues/612)) ([3eef795](https://github.com/wheregmis/threadlane/commit/3eef795c337bd621d65922b88f71032828ca830a))
* **hub:** keep millisecond precision in dream task updated_at ([72fbd76](https://github.com/wheregmis/threadlane/commit/72fbd7616d5691effa9462f0f89d721698225ed5))
* **hub:** Lead priority, tier 3 rule offers, keep-awake and honest operations metrics ([cb5b711](https://github.com/wheregmis/threadlane/commit/cb5b711ec38b2a93abf06bdb247bea853872bcbb))
* **hub:** Let project deletion proceed during GitHub reads ([9a61789](https://github.com/wheregmis/threadlane/commit/9a617898070a177368f72c6ebdbefc260b80c85a))
* **hub:** Let review runs read the reviewed project's memory ([#610](https://github.com/wheregmis/threadlane/issues/610)) ([f81400c](https://github.com/wheregmis/threadlane/commit/f81400c4d0d327e98a1f8503e29eb22669ec9d65))
* **hub:** list_tasks keeps the most recently finished tasks by end time ([382a02f](https://github.com/wheregmis/threadlane/commit/382a02fbc56eac3bdddb8f456d726e2d261f4948))
* **hub:** match task-review approvals on the canonical PR, not the registration ([acf8fd5](https://github.com/wheregmis/threadlane/commit/acf8fd56bd91f7f96c8672f1c4f652e560076036))
* **hub:** Offer a tier 3 standing rule only where it applies ([c096071](https://github.com/wheregmis/threadlane/commit/c09607121244a3077be53398068dbf74524bd8ac))
* **hub:** page large agent-facing tool results ([#563](https://github.com/wheregmis/threadlane/issues/563)) ([486e7ad](https://github.com/wheregmis/threadlane/commit/486e7ad38429e3ccd7575b0f1d0ca5639357383b))
* **hub:** Read tracked-PR activity outside the removal lock ([#620](https://github.com/wheregmis/threadlane/issues/620)) ([1a3f4da](https://github.com/wheregmis/threadlane/commit/1a3f4dadba26a907da7c40d1dc0c3c88ab2ef30a))
* **hub:** recover stuck tasks and fast-forward existing PRs ([788edf6](https://github.com/wheregmis/threadlane/commit/788edf617951ad448d7118a21407112a01d19749))
* **hub:** reject Antigravity skill bindings ([d068535](https://github.com/wheregmis/threadlane/commit/d068535f6825986de6464cac05f9b815512a9b0d))
* **hub:** remove the most frequent agent tool hiccups found in the dogfood data ([7dc6073](https://github.com/wheregmis/threadlane/commit/7dc607341d423a951daef0a146c3837d6e9078ba))
* **hub:** reuse PR reviews for the same head commit ([6045179](https://github.com/wheregmis/threadlane/commit/604517977a80a71696bd03905e59fe688ef528a4))
* **hub:** Say what changed in tracked-PR notifications ([#620](https://github.com/wheregmis/threadlane/issues/620)) ([fdbcba2](https://github.com/wheregmis/threadlane/commit/fdbcba2ba3f1937bacc88a9c97515f8765ef7add))
* **hub:** select scan reviewers and complete standalone PR verdicts ([debc55e](https://github.com/wheregmis/threadlane/commit/debc55e3571fe2d3c3da54d579aa2fd4e52f5b23))
* **hub:** tell a run its task was cancelled, and name the status when finish_task is refused ([ed842d2](https://github.com/wheregmis/threadlane/commit/ed842d2def93be54216158ac78aacdb39831aecc)), closes [#648](https://github.com/wheregmis/threadlane/issues/648)
* pin publish approval to the previewed commit ([81dbf2d](https://github.com/wheregmis/threadlane/commit/81dbf2dbe06a7cead40a5be707e9a9e45b0aa249))
* preserve workflow retries and render usage task links ([a4bcb5a](https://github.com/wheregmis/threadlane/commit/a4bcb5ae9524e66028a7ee644f2b4aaa93e8542c))
* **runner,antigravity:** hold tasks on quota and fall back to the default model ([335eb1e](https://github.com/wheregmis/threadlane/commit/335eb1eb276db3a3c5d1fcc5d3015e9277e53fd2))
* **runner:** Check the live branch ref before deleting a closed PR's branch ([10f29ee](https://github.com/wheregmis/threadlane/commit/10f29ee2afc2342fec38fa16e1e6b0e49f84320b))
* **runner:** collapse deferred-tool guard into match arm for clippy 1.95 ([fa9243e](https://github.com/wheregmis/threadlane/commit/fa9243e81aa15c3767b7361d3e2a5ee03fd2fafd))
* **runner:** isolate standalone PR review checkouts ([4ddfc88](https://github.com/wheregmis/threadlane/commit/4ddfc88911c4295bd397c9ba2c824b3eae10f65f))
* **runner:** Tell a task where its work from before a repository was attached is ([#616](https://github.com/wheregmis/threadlane/issues/616)) ([68ca275](https://github.com/wheregmis/threadlane/commit/68ca275feb9c89cd15a738497f7ba097e2e559d5))
* **skills:** address [#559](https://github.com/wheregmis/threadlane/issues/559) review — no skills on extra turns, plain codex app-server ([7001d72](https://github.com/wheregmis/threadlane/commit/7001d7220c6306b49922261903791468aa328962))
* **skills:** read-only card skills, partial skill listing, no skills on lead runs ([4369cbf](https://github.com/wheregmis/threadlane/commit/4369cbf4b4ca7b6f83b77506d7236403642cd053))
* **skills:** request a single Claude skill by name instead of a leading slash command ([1a36185](https://github.com/wheregmis/threadlane/commit/1a36185fab0fdb19613d6a2d9f41f80302ac274e))
* **skills:** request a single Claude skill by name, not as a leading slash command ([f2af2fe](https://github.com/wheregmis/threadlane/commit/f2af2fefe9ae946d14f359733fa13eb5fc9f6a50))
* **store,desktop:** Make operations metrics say what they mean ([01e3f7e](https://github.com/wheregmis/threadlane/commit/01e3f7e23e4ab800384a0bb3425d3aee346fb4fa))
* **store:** count session running totals once per run in usage ([d6f1a84](https://github.com/wheregmis/threadlane/commit/d6f1a84e2914a16d01efb7e1ad8233f0e9a05003))
* **store:** Count session running totals once per run in usage ([5920804](https://github.com/wheregmis/threadlane/commit/59208040e5c925f4a5c4f38d9d47e1c9d9f66c20))
* **store:** Offer lead lanes before task lanes when placing work ([70ffd7c](https://github.com/wheregmis/threadlane/commit/70ffd7c22833c2e29c1cb3121a6f01121827c8e4))
* **store:** Put only lead lanes you are waiting on ahead of task lanes ([55c6d22](https://github.com/wheregmis/threadlane/commit/55c6d2258739e6b66ecc9e5cc2634713d4b9c04b))
* treat run once as an explicit owner request ([b3b4a89](https://github.com/wheregmis/threadlane/commit/b3b4a894a9822bdfbdf7f083d9b0d20018be3c36))


### Performance Improvements

* **desktop:** cache parsed Prose markdown and cap project chat sidebar tasks ([530f39d](https://github.com/wheregmis/threadlane/commit/530f39dd3e2a5cd3271be5f8aa3c55d06c154362))
* **desktop:** Preserve cached chat projections while typing ([9454d35](https://github.com/wheregmis/threadlane/commit/9454d35fd23c8aea73a903f5e5bfc1367948b58e))
* **desktop:** Refresh paired hubs concurrently ([66c8d19](https://github.com/wheregmis/threadlane/commit/66c8d19ca6dc1d0a93641cafd3564688a1bf4008))
* **desktop:** Reuse ordered projections for long chats ([f82466e](https://github.com/wheregmis/threadlane/commit/f82466effbc8af6b87ca97124559200f04dc696f))
* **hub:** eliminate hub/store/runner bottlenecks and enable UI max_parallel selection ([f648919](https://github.com/wheregmis/threadlane/commit/f6489197f08a4d1920828a5a79ebc85d220650c4))
* **hub:** Page agent task listings in the database ([d8203ea](https://github.com/wheregmis/threadlane/commit/d8203eae269286370ade201f7d0aa21ed4a15401))
* reduce desktop refresh and journal overhead ([2e32280](https://github.com/wheregmis/threadlane/commit/2e3228073f7e0fb9f5f98e8704bcb35d846e9800))


### CI

* Compare Rust cache with Mr. Boxington on Linux ([f4a9183](https://github.com/wheregmis/threadlane/commit/f4a91835659ba55b9b8e753d3f443a1b9373f3f9))
* Use Mr. Boxington for Ubuntu checks ([c1635a1](https://github.com/wheregmis/threadlane/commit/c1635a1cf07aae57c98b52d639469310db6927bc))
* Use Mr. Boxington on Ubuntu and fix hub-lock flake ([d111ab7](https://github.com/wheregmis/threadlane/commit/d111ab70d44b6a53d9d9f1680c6809e1a0c984d3))


### Maintenance

* **store:** renumber review configuration migration to 0031 ([a84be8b](https://github.com/wheregmis/threadlane/commit/a84be8bb4ab8a8e7af9468738d5cd8d9a0885e34))

## [0.1.54](https://github.com/wheregmis/threadlane/compare/v0.1.53...v0.1.54) (2026-11-19)


### Features

* add Antigravity CLI backend ([#531](https://github.com/wheregmis/threadlane/issues/531)) ([87da207](https://github.com/wheregmis/threadlane/commit/87da20778d08badcf11e2c661f7cb3e7887688f6))
* ask UI agents to check their change in the real window ([#543](https://github.com/wheregmis/threadlane/issues/543)) ([5f32178](https://github.com/wheregmis/threadlane/commit/5f321781fad3c6d44de84caa708080eb3ad70b57))
* fail a run at once when the backend refuses its model ([#538](https://github.com/wheregmis/threadlane/issues/538)) ([5f32178](https://github.com/wheregmis/threadlane/commit/5f321781fad3c6d44de84caa708080eb3ad70b57))
* give runs room to finish and tell a retry to continue ([#539](https://github.com/wheregmis/threadlane/issues/539)) ([5f32178](https://github.com/wheregmis/threadlane/commit/5f321781fad3c6d44de84caa708080eb3ad70b57))
* let a new task end by opening a draft pull request ([#541](https://github.com/wheregmis/threadlane/issues/541)) ([5f32178](https://github.com/wheregmis/threadlane/commit/5f321781fad3c6d44de84caa708080eb3ad70b57))
* let agents mark a draft pull request ready for review ([988497b](https://github.com/wheregmis/threadlane/commit/988497b628dbba6d6e3c342c50edba944127633d))
* remind once about a decision still waiting after ten minutes ([#544](https://github.com/wheregmis/threadlane/issues/544)) ([5f32178](https://github.com/wheregmis/threadlane/commit/5f321781fad3c6d44de84caa708080eb3ad70b57))
* retry a failed task in one click ([#540](https://github.com/wheregmis/threadlane/issues/540)) ([5f32178](https://github.com/wheregmis/threadlane/commit/5f321781fad3c6d44de84caa708080eb3ad70b57))
* wake a task on CI of a pull request it commented on or merged ([#542](https://github.com/wheregmis/threadlane/issues/542)) ([5f32178](https://github.com/wheregmis/threadlane/commit/5f321781fad3c6d44de84caa708080eb3ad70b57))


### Bug Fixes

* claim a reminder only while its decision is pending ([a3aa0ab](https://github.com/wheregmis/threadlane/commit/a3aa0abbc3429121fd29ad070e8928226d8d3212))
* refuse a second registration of the same repository ([c2018ad](https://github.com/wheregmis/threadlane/commit/c2018ad3eb9264cef0d60358cceeca9e96ac5000))
* reject a draft pull request task without a repository ([ab7c3eb](https://github.com/wheregmis/threadlane/commit/ab7c3ebb2f3700798d6b4a13debb9db147f68461))
* send a decision's reminder once across restarts ([179c5a2](https://github.com/wheregmis/threadlane/commit/179c5a2b9c5d91a340892989fe9b1be43a59ea5a))


### Performance Improvements

* bound prompt excerpt work and avoid temporary transcript rows ([ebb689d](https://github.com/wheregmis/threadlane/commit/ebb689df323471acde3b787806daadc55480d21c))


### CI

* restore Release Please and Hotpath benchmarks ([7f14a01](https://github.com/wheregmis/threadlane/commit/7f14a018a3e610eb7dc0bbdffb5a8cb085648228))


### Maintenance

* **deps:** bump hotpath from 0.29.2 to 0.30.3 ([9664a2f](https://github.com/wheregmis/threadlane/commit/9664a2f287320d2f2e797e7122cc0155edcbf2fc))
* **deps:** bump hotpath-rs/hotpath/.github/workflows/hotpath-relay.yml ([1503cf7](https://github.com/wheregmis/threadlane/commit/1503cf79d6e8ba89932abf46f56e4e42f781d4ec))
* **deps:** bump hotpath-rs/hotpath/.github/workflows/hotpath-relay.yml from 0.29.2 to 0.30.3 ([88b5547](https://github.com/wheregmis/threadlane/commit/88b554711d72a630e64c97c55677b59a8efcff18))
* **deps:** bump log from 0.4.34 to 0.4.35 ([6df91b3](https://github.com/wheregmis/threadlane/commit/6df91b3b54ccb5a2b6e16d3292f96fe67d35882b))
* **deps:** bump notify from 8.2.0 to 8.2.1 ([4555523](https://github.com/wheregmis/threadlane/commit/45555237bade20e1271bca351367b6748d3891d3))
* **deps:** bump robius-open from `aea7a4f` to `9b235c3` ([6d4b28f](https://github.com/wheregmis/threadlane/commit/6d4b28fc093d68a49f4f55060c990170f3dfb2d6))
* **deps:** bump rustix from 1.1.5 to 1.1.6 ([5ad152f](https://github.com/wheregmis/threadlane/commit/5ad152f28db93b4a82465dd41a0ea6c2c7463819))
* **deps:** bump tokio from 1.53.2 to 1.53.3 ([b8255f4](https://github.com/wheregmis/threadlane/commit/b8255f48fa9a8fa051bdeb40e02ce2080bcc8a0b))
* **deps:** bump tokio from 1.54.0 to 1.54.1 ([6ca16c1](https://github.com/wheregmis/threadlane/commit/6ca16c1daef3c52fa38c11c50a219e4a939923bf))
* Fix hotpath-cloud workflow ([563b3c9](https://github.com/wheregmis/threadlane/commit/563b3c9ddae69d70968dd198c9b76cbf66c61a38))

## [0.1.53 and earlier](old/CHANGELOG.md)

See the archived changelog for releases through v0.1.53.
