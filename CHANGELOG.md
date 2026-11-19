# Changelog

## [0.1.54](https://github.com/wheregmis/threadlane/compare/v0.1.53...v0.1.54) (2026-11-19)


### Features

* add Antigravity CLI backend ([#531](https://github.com/wheregmis/threadlane/issues/531)) ([87da207](https://github.com/wheregmis/threadlane/commit/87da20778d08badcf11e2c661f7cb3e7887688f6))
* ask UI agents to check their change in the real window ([#543](https://github.com/wheregmis/threadlane/issues/543)) ([5f32178](https://github.com/wheregmis/threadlane/commit/5f321781fad3c6d44de84caa708080eb3ad70b57))
* fail a run at once when the backend refuses its model ([#538](https://github.com/wheregmis/threadlane/issues/538)) ([5f32178](https://github.com/wheregmis/threadlane/commit/5f321781fad3c6d44de84caa708080eb3ad70b57))
* give runs room to finish and tell a retry to continue ([#539](https://github.com/wheregmis/threadlane/issues/539)) ([5f32178](https://github.com/wheregmis/threadlane/commit/5f321781fad3c6d44de84caa708080eb3ad70b57))
* let a new task end by opening a draft pull request ([#541](https://github.com/wheregmis/threadlane/issues/541)) ([5f32178](https://github.com/wheregmis/threadlane/commit/5f321781fad3c6d44de84caa708080eb3ad70b57))
* let agents mark a draft pull request ready for review ([4c4b5d0](https://github.com/wheregmis/threadlane/commit/4c4b5d0952f33ee48fdc9b3001246108cb2cc8f5))
* let agents mark a draft pull request ready for review ([988497b](https://github.com/wheregmis/threadlane/commit/988497b628dbba6d6e3c342c50edba944127633d))
* remind once about a decision still waiting after ten minutes ([#544](https://github.com/wheregmis/threadlane/issues/544)) ([5f32178](https://github.com/wheregmis/threadlane/commit/5f321781fad3c6d44de84caa708080eb3ad70b57))
* retry a failed task in one click ([#540](https://github.com/wheregmis/threadlane/issues/540)) ([5f32178](https://github.com/wheregmis/threadlane/commit/5f321781fad3c6d44de84caa708080eb3ad70b57))
* wake a task on CI of a pull request it commented on or merged ([#542](https://github.com/wheregmis/threadlane/issues/542)) ([5f32178](https://github.com/wheregmis/threadlane/commit/5f321781fad3c6d44de84caa708080eb3ad70b57))


### Bug Fixes

* claim a reminder only while its decision is pending ([a3aa0ab](https://github.com/wheregmis/threadlane/commit/a3aa0abbc3429121fd29ad070e8928226d8d3212))
* refuse a second registration of the same repository ([766a4b3](https://github.com/wheregmis/threadlane/commit/766a4b3bb4d6cf32314383f9f3c5747eabf08e9e))
* refuse a second registration of the same repository ([c2018ad](https://github.com/wheregmis/threadlane/commit/c2018ad3eb9264cef0d60358cceeca9e96ac5000))
* reject a draft pull request task without a repository ([d6aaba8](https://github.com/wheregmis/threadlane/commit/d6aaba826e7ca8583a2950e55fa5893144178d92))
* reject a draft pull request task without a repository ([ab7c3eb](https://github.com/wheregmis/threadlane/commit/ab7c3ebb2f3700798d6b4a13debb9db147f68461))
* send a decision's reminder once across restarts ([10e417f](https://github.com/wheregmis/threadlane/commit/10e417f22a92db1fbc44a333bca4973dea1e7b61))
* send a decision's reminder once across restarts ([179c5a2](https://github.com/wheregmis/threadlane/commit/179c5a2b9c5d91a340892989fe9b1be43a59ea5a))


### Performance Improvements

* bound prompt excerpt work and avoid temporary transcript rows ([ebb689d](https://github.com/wheregmis/threadlane/commit/ebb689df323471acde3b787806daadc55480d21c))


### CI

* restore Release Please and Hotpath benchmarks ([79cc19a](https://github.com/wheregmis/threadlane/commit/79cc19a421783dbb9fdd6230129e9da93d974bd5))
* restore Release Please and Hotpath benchmarks ([7f14a01](https://github.com/wheregmis/threadlane/commit/7f14a018a3e610eb7dc0bbdffb5a8cb085648228))


### Maintenance

* **deps:** bump hotpath from 0.29.2 to 0.30.3 ([25e765e](https://github.com/wheregmis/threadlane/commit/25e765eb58d527d74224df4199a79d0773ced309))
* **deps:** bump hotpath from 0.29.2 to 0.30.3 ([9664a2f](https://github.com/wheregmis/threadlane/commit/9664a2f287320d2f2e797e7122cc0155edcbf2fc))
* **deps:** bump hotpath-rs/hotpath/.github/workflows/hotpath-relay.yml ([1503cf7](https://github.com/wheregmis/threadlane/commit/1503cf79d6e8ba89932abf46f56e4e42f781d4ec))
* **deps:** bump hotpath-rs/hotpath/.github/workflows/hotpath-relay.yml from 0.29.2 to 0.30.3 ([88b5547](https://github.com/wheregmis/threadlane/commit/88b554711d72a630e64c97c55677b59a8efcff18))
* **deps:** bump log from 0.4.34 to 0.4.35 ([b46c5bb](https://github.com/wheregmis/threadlane/commit/b46c5bb0b96c0d16d98a295f3f126ddd4902cd0c))
* **deps:** bump log from 0.4.34 to 0.4.35 ([6df91b3](https://github.com/wheregmis/threadlane/commit/6df91b3b54ccb5a2b6e16d3292f96fe67d35882b))
* **deps:** bump notify from 8.2.0 to 8.2.1 ([ec7e974](https://github.com/wheregmis/threadlane/commit/ec7e974964393c94a6ee7351fe8671a7fc6da307))
* **deps:** bump notify from 8.2.0 to 8.2.1 ([4555523](https://github.com/wheregmis/threadlane/commit/45555237bade20e1271bca351367b6748d3891d3))
* **deps:** bump robius-open from `aea7a4f` to `9b235c3` ([307e80b](https://github.com/wheregmis/threadlane/commit/307e80b779ff512cadf21f04f4337318c2b0281d))
* **deps:** bump robius-open from `aea7a4f` to `9b235c3` ([6d4b28f](https://github.com/wheregmis/threadlane/commit/6d4b28fc093d68a49f4f55060c990170f3dfb2d6))
* **deps:** bump rustix from 1.1.5 to 1.1.6 ([435090f](https://github.com/wheregmis/threadlane/commit/435090f50bc603d36e105190945507e1412bb6cb))
* **deps:** bump rustix from 1.1.5 to 1.1.6 ([5ad152f](https://github.com/wheregmis/threadlane/commit/5ad152f28db93b4a82465dd41a0ea6c2c7463819))
* **deps:** bump tokio from 1.53.2 to 1.53.3 ([b271082](https://github.com/wheregmis/threadlane/commit/b2710821c5d6c748a57ee1e9299e603ecb19791b))
* **deps:** bump tokio from 1.53.2 to 1.53.3 ([b8255f4](https://github.com/wheregmis/threadlane/commit/b8255f48fa9a8fa051bdeb40e02ce2080bcc8a0b))
* **deps:** bump tokio from 1.54.0 to 1.54.1 ([c7706a8](https://github.com/wheregmis/threadlane/commit/c7706a8abdf95537bf0ed4178986bdfd345ba792))
* **deps:** bump tokio from 1.54.0 to 1.54.1 ([6ca16c1](https://github.com/wheregmis/threadlane/commit/6ca16c1daef3c52fa38c11c50a219e4a939923bf))
* Fix hotpath-cloud workflow ([be1dec2](https://github.com/wheregmis/threadlane/commit/be1dec2f5263620c65993a78e669ec3c2f2b5f0e))
* Fix hotpath-cloud workflow ([563b3c9](https://github.com/wheregmis/threadlane/commit/563b3c9ddae69d70968dd198c9b76cbf66c61a38))

## [0.1.53 and earlier](old/CHANGELOG.md)

See the archived changelog for releases through v0.1.53.
