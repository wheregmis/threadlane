# Changelog

## [0.1.56](https://github.com/wheregmis/threadlane/compare/v0.1.55...v0.1.56) (2026-11-21)


### Bug Fixes

* **hub:** give review runs the task lane's memory tools ([#623](https://github.com/wheregmis/threadlane/issues/623)) ([df98403](https://github.com/wheregmis/threadlane/commit/df98403301636acc95622418c523b6ff0d1631ea))
* **hub:** give review runs the task lane's memory tools ([#623](https://github.com/wheregmis/threadlane/issues/623)) ([c657f0d](https://github.com/wheregmis/threadlane/commit/c657f0dcb7ad088a18dd41238ff9c235fb6822fd))
* **hub:** offer pr_review_status to review runs ([#682](https://github.com/wheregmis/threadlane/issues/682)) ([cbe0ada](https://github.com/wheregmis/threadlane/commit/cbe0ada0f29c28a9dc83f43792f9a6c738f93507))
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
