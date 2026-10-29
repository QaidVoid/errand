# Changelog

What changed in each release, from the commit messages.
## [0.4.0] - 2026-10-29


### Features
- (broker) Refuse an unlisted host and let the owner grant it ([#107](https://github.com/QaidVoid/errand/pull/107)) ([79b6aaa](https://github.com/QaidVoid/errand/commit/79b6aaaaa28b4acc3aba955ee2a4c59a87074718))
- (broker) Hold a provider key to its header and declared calls ([#106](https://github.com/QaidVoid/errand/pull/106)) ([0458261](https://github.com/QaidVoid/errand/commit/0458261650c8dfec32bf42680f0f1a6fe66c11ed))
- (broker) Keep a receipt of every brokered provider call ([#101](https://github.com/QaidVoid/errand/pull/101)) ([31efaa5](https://github.com/QaidVoid/errand/commit/31efaa53aa13c5903e6cf95d13efbbf5c7420011))
- (chat) List models as provider/model in !model ([d8c1248](https://github.com/QaidVoid/errand/commit/d8c1248298bf0c6304ff11aa9ad40721fd51ca88))
- (commands) Add !gen-speech to read a text aloud ([35c8320](https://github.com/QaidVoid/errand/commit/35c8320a2b897727f5c21535c1a1e87dfc8491cf))
- (config) Give sessions kage's edit and music tools ([38774bd](https://github.com/QaidVoid/errand/commit/38774bd79e0903b7579a001f3433e612b9afaa28))
- (config) Give sessions kage's speech tool ([bbb9afb](https://github.com/QaidVoid/errand/commit/bbb9afbfc75e6a68c871dd7b17cadaf2b46e05c2))
- (github) Mint each session an App token in place of the PAT ([#108](https://github.com/QaidVoid/errand/pull/108)) ([7aa20df](https://github.com/QaidVoid/errand/commit/7aa20dfb2fd4fa168becdc1970a5fad407f4e975))
- (session) [breaking] Give every session a project of its own ([#110](https://github.com/QaidVoid/errand/pull/110)) ([9e95a04](https://github.com/QaidVoid/errand/commit/9e95a04b8662008e96d884153503a75b42d214c6))
- (session) Collect what a cold session left on disk ([#105](https://github.com/QaidVoid/errand/pull/105)) ([d9a4224](https://github.com/QaidVoid/errand/commit/d9a422427ac341073826d06252ca619765a68079))
- (session) Post the clips the agent makes ([773a886](https://github.com/QaidVoid/errand/commit/773a8865351b04809360cd03e03773ba22a3f3fe))
- (session) Show what a running tool call is doing ([9e1be92](https://github.com/QaidVoid/errand/commit/9e1be92b807c97f17098086a8fce32d8ea93e83b))
- (threads) Post what was said on the web in the thread ([ec52f98](https://github.com/QaidVoid/errand/commit/ec52f981a6c6a59f398dd4e5c8007fde22bbfc39))
- (web) Start from a home composer with model and command pickers ([45f0af6](https://github.com/QaidVoid/errand/commit/45f0af67b1b5ade29beff69d6f183154ec24d6d1))
- (web) Pick a session's model and complete commands ([3c467d1](https://github.com/QaidVoid/errand/commit/3c467d14c32a0b51cc04b94b36a8e90aaa32616e))
- (web) Log in with Discord to act as your own account ([56540fc](https://github.com/QaidVoid/errand/commit/56540fc40b6a25ed080fd6be813444a8b70ac825))
- Address a channel picture to whoever asked for it ([715e9b0](https://github.com/QaidVoid/errand/commit/715e9b0b777a54d3f8e7dc7a9e358945c6798dc3))
- Let !gen-image name an effort ([38323ab](https://github.com/QaidVoid/errand/commit/38323ab48c38d2c2f4c92d6c3c82032e2f3a0067))
- Add !gen-image to run one prompt on the image model ([b87e048](https://github.com/QaidVoid/errand/commit/b87e048b748a354911231a1a7036a917f3d89c75))
- Generate images and post them to the thread ([#35](https://github.com/QaidVoid/errand/pull/35)) ([4a8011c](https://github.com/QaidVoid/errand/commit/4a8011cd3668b94a72aea9dbffbd2d3ab16d4f18))

### Fixes
- (broker) Judge the dial, follow no redirect, require HTTPS ([#70](https://github.com/QaidVoid/errand/pull/70)) ([e876928](https://github.com/QaidVoid/errand/commit/e87692814c54f6f5c9ba970b62a01c1afbcbf5f6))
- (broker) Scrub the credential from what a provider answers ([#69](https://github.com/QaidVoid/errand/pull/69)) ([f108fd5](https://github.com/QaidVoid/errand/commit/f108fd5cdf48c9a05caf47c23edcab560cdb155c))
- (broker) Mint legible session secrets and scrub them by shape ([#68](https://github.com/QaidVoid/errand/pull/68)) ([c963f29](https://github.com/QaidVoid/errand/commit/c963f29c438b015f0278663aeb9e28a742275ccc))
- (broker) Give each session a broker and nonces of its own ([#67](https://github.com/QaidVoid/errand/pull/67)) ([9820a7e](https://github.com/QaidVoid/errand/commit/9820a7eae871a6877bca42de88242f55206ce38d))
- (broker) Close refusals explicitly so git retries a 407 ([e9737af](https://github.com/QaidVoid/errand/commit/e9737af6983da49cbe6096c0b23091a81675f0e3))
- (broker) Require a proxy credential on CONNECT and forwards ([#66](https://github.com/QaidVoid/errand/pull/66)) ([ea629b7](https://github.com/QaidVoid/errand/commit/ea629b73060dc55f3897a0a9d5ce697abf21e118))
- (broker) Confine provider requests to configured origins ([#58](https://github.com/QaidVoid/errand/pull/58)) ([3ae5050](https://github.com/QaidVoid/errand/commit/3ae50503b8cda8d880be7a2a1253467139ed3a31))
- (chat) Make free-text upload captions inert ([#60](https://github.com/QaidVoid/errand/pull/60)) ([c73fe66](https://github.com/QaidVoid/errand/commit/c73fe664252a800f5cd9f0b68b6a44563cd7bcef))
- (commands) Narrow the help column to leave room under 2000 ([18308eb](https://github.com/QaidVoid/errand/commit/18308eb55c599d454e8f231ea54fcdf71eaa3e9d))
- (config) Refuse a public interface that logs nobody in ([57b26d0](https://github.com/QaidVoid/errand/commit/57b26d015ef58b37665739da95ddd97f7e0cfe3f))
- (config) Reject unreachable image configurations ([#62](https://github.com/QaidVoid/errand/pull/62)) ([afdd54e](https://github.com/QaidVoid/errand/commit/afdd54e88f962dab5892a8dddb7651fbc22da136))
- (provider) Stop reading an image answer at its cap ([ebc31ba](https://github.com/QaidVoid/errand/commit/ebc31bab8e297ecfbf25c26136c5f9215d3af82d))
- (sandbox) Broker podman sessions and keep secrets off the argv ([#103](https://github.com/QaidVoid/errand/pull/103)) ([40252d6](https://github.com/QaidVoid/errand/commit/40252d68d8b94058ab48ad60c12f10c8a099932a))
- (sandbox) Say what egress allows and refuse the Azure fabric ([#98](https://github.com/QaidVoid/errand/pull/98)) ([10ed2b2](https://github.com/QaidVoid/errand/commit/10ed2b21868d80a1b96659547b0896a365477daf))
- (sandbox) Ask kage which provider ids it knows ([#64](https://github.com/QaidVoid/errand/pull/64)) ([5bcfee1](https://github.com/QaidVoid/errand/commit/5bcfee1aa455d550b84e90147b3e9fdca2a6a75a))
- (sandbox) Use the google built-in provider id ([#61](https://github.com/QaidVoid/errand/pull/61)) ([d3d67c6](https://github.com/QaidVoid/errand/commit/d3d67c6dc3f979b9cfbdbd78305e5e4407ecd508))
- (session) Report a failed prompt as a failed turn ([88d666d](https://github.com/QaidVoid/errand/commit/88d666d431a1d3d4b5da1507bde32aa435bfa8c6))
- (session) Push from a daemon-owned copy that borrows nothing ([#102](https://github.com/QaidVoid/errand/pull/102)) ([1df9f51](https://github.com/QaidVoid/errand/commit/1df9f51f5d58bd42093992f401cedb6f5efeedbe))
- (session) Bound every read of a file the agent can grow ([#100](https://github.com/QaidVoid/errand/pull/100)) ([2e6eda5](https://github.com/QaidVoid/errand/commit/2e6eda5dbd643f6fcbecc764606c46cd8616cb38))
- (session) Refuse uploads that hold a secret, floor short tokens ([#99](https://github.com/QaidVoid/errand/pull/99)) ([a061819](https://github.com/QaidVoid/errand/commit/a0618199b7e3a651f5aa1bbbb9e507232de59c1a))
- (session) Wake a stopped thread only for who may take part ([1ccb1b3](https://github.com/QaidVoid/errand/commit/1ccb1b3ab736bdd836c2f58d217ba67b63b3d368))
- (session) Fetch a song the agent could only link to ([da7cb43](https://github.com/QaidVoid/errand/commit/da7cb431b1776166dbc1c48d4dfa30c7c35f6fa3))
- (session) Tell the agent the image cap stopped its turn ([#65](https://github.com/QaidVoid/errand/pull/65)) ([746a943](https://github.com/QaidVoid/errand/commit/746a943be639a1c6c173486d737ab9a3e9d63287))
- (session) Interrupt turns that exceed the image cap ([#63](https://github.com/QaidVoid/errand/pull/63)) ([997f1ca](https://github.com/QaidVoid/errand/commit/997f1ca76727ba623982918524d8c75fb7c56c73))
- (session) Refuse symlinks when reading the agent session id ([#59](https://github.com/QaidVoid/errand/pull/59)) ([1ace712](https://github.com/QaidVoid/errand/commit/1ace712f4e70b7020bce307412a1f161a3fecce4))
- (session) Keep a session alive while a tool call runs ([65adcde](https://github.com/QaidVoid/errand/commit/65adcde9168ded3f4a01855248d0dbe7c250750e))
- (session) Name the providers a shared model id is served by ([0f48104](https://github.com/QaidVoid/errand/commit/0f481049f8e4864363236c0975784df8c6fe4fd3))
- (session) Quote the failing line when a sandbox runs out of disk ([206fc5a](https://github.com/QaidVoid/errand/commit/206fc5ae53f53e645731f706dae327251c9247cf))
- (web) Name login cookies so sibling hosts cannot set them ([ddd20b0](https://github.com/QaidVoid/errand/commit/ddd20b08953c29222ef20b10a6be609d7e34a957))
- (web) Read projects and transcripts off the async workers ([979fee3](https://github.com/QaidVoid/errand/commit/979fee3a6bb0c4d0782acf47ba2ac7392282ceec))
- (web) Ask again whether a visitor is still in the server ([d34a4ba](https://github.com/QaidVoid/errand/commit/d34a4ba876c24495fb817dead33d973dba5d2468))
- (web) Harden the interface against strangers ([012538b](https://github.com/QaidVoid/errand/commit/012538be07d2d3afb14daf767802613feece9d2a))
- (web) Show when each thing happened, not when it was replayed ([bf7b611](https://github.com/QaidVoid/errand/commit/bf7b61172515641971781c42cfc908e43c8b94ef))
- Harden transcript links, bailey XDG and late provider keys ([#109](https://github.com/QaidVoid/errand/pull/109)) ([9935da9](https://github.com/QaidVoid/errand/commit/9935da9203c5a99b1c30b0252f95b864f7d5ff9e))
- Stop the agent memory reaching argv and name kills ([759f05a](https://github.com/QaidVoid/errand/commit/759f05ad360094b8b176a6ece0bd8963b2ff854d))
- Resolve an image model to the provider that lists it ([ad61174](https://github.com/QaidVoid/errand/commit/ad611748f1297eedc1e1df0489df54245df216e5))
- Draw with the images endpoint, not a chat model ([29d046c](https://github.com/QaidVoid/errand/commit/29d046c3280d4a304c0752326ab58196ff154d27))

### Tests
- (sandbox) Check a brokered bailey session on the real tool ([#104](https://github.com/QaidVoid/errand/pull/104)) ([cef95ee](https://github.com/QaidVoid/errand/commit/cef95eee15e59c6b0c9892e69eb0e5914b6e7e69))
- Wait for the session to go idle instead of sleeping ([#111](https://github.com/QaidVoid/errand/pull/111)) ([60525a6](https://github.com/QaidVoid/errand/commit/60525a67989f49070af88e93a07a799038c35321))

## [0.3.1] - 2026-10-18


### Features
- (config) Make !file upload limit configurable ([6ae99e1](https://github.com/QaidVoid/errand/commit/6ae99e186c3689dd7c72c2ab219c42211e4107fd))

### Fixes
- (pr) Open pull requests against the forked-from repository ([86cd3fc](https://github.com/QaidVoid/errand/commit/86cd3fccd317c9ec1815f82e189f93fda9cdbf3f))
- (session) End the session task once the session ends ([#30](https://github.com/QaidVoid/errand/pull/30)) ([c3da504](https://github.com/QaidVoid/errand/commit/c3da504b6b6cd3da88e28da89e10dbaf76c5f886))
- (usage) Read kage's token totals and show tok/s ([#31](https://github.com/QaidVoid/errand/pull/31)) ([f47a0c3](https://github.com/QaidVoid/errand/commit/f47a0c31f99d7d3a43399b28764aac2410bc6860))
- (usage) Let a mapped provider bill past a spent window ([#28](https://github.com/QaidVoid/errand/pull/28)) ([880c34d](https://github.com/QaidVoid/errand/commit/880c34d3709b49ceda4df5e2b7964514ce8373e7))

## [0.3.0] - 2026-10-10


### Features
- (chat) Take discord stickers as attachments ([e442f96](https://github.com/QaidVoid/errand/commit/e442f962194863525627aea4360bc24f5bc932c1))
- (providers) Pass model sizes and broker anthropic keys ([fb97456](https://github.com/QaidVoid/errand/commit/fb97456c2ef922fc298161a9389eab7941ffcd3a))
- (providers) Honor kind for custom kage providers ([651adc5](https://github.com/QaidVoid/errand/commit/651adc5288a68a254fac0c14e8d412751d6797a5))
- Register kage plugins and leave them unbrokered by default ([92bb231](https://github.com/QaidVoid/errand/commit/92bb23127bc3cd810aa61975104089517f05ba2e))
- Run sessions on kage rpc instead of pi ([c4110ab](https://github.com/QaidVoid/errand/commit/c4110abf80c0402234d887e0bd86e341d3d113ae))

### Fixes
- (agent) Accept a resume answer without a session id ([28f78e6](https://github.com/QaidVoid/errand/commit/28f78e60c44eb7d7258e8f88d6a5d6428ef6a70c))
- (pr) Open fork pull requests against the parent ([ff9abe3](https://github.com/QaidVoid/errand/commit/ff9abe3645c8d4f60c5c90178a817c6ed0a2d5c8))
- Run unconfigured tools without asking in sessions ([f16179a](https://github.com/QaidVoid/errand/commit/f16179a876fe8b316ecde781a2cf4686244b0adf))
- Render tool commands and whole spans of agent speech ([6914dc0](https://github.com/QaidVoid/errand/commit/6914dc0de00fd5546754fe7aeb541a66ea0ea021))
- Widen the session resolver to three public resolvers ([02122a8](https://github.com/QaidVoid/errand/commit/02122a847b8bff2f7f0f4d627bacb8e2de7e061a))

### Refactoring
- (agent) Drive sessions over kage ACP at pi parity ([7402932](https://github.com/QaidVoid/errand/commit/7402932e618987b62850bfb98f0af8115ade606e))

## [0.2.5] - 2026-09-30


### Fixes
- (sandbox) Fix shell tool spawn failing ([1168577](https://github.com/QaidVoid/errand/commit/116857731ddbea465bb95e43ca92f317e52630d0))

## [0.2.4] - 2026-09-29


### Features
- Add errand skills ([b90c1c7](https://github.com/QaidVoid/errand/commit/b90c1c7a8d42e351353c91715717d910a3b7b7e2))

### Fixes
- (sandbox) Bound the broker's dial across all addresses ([ace9ba7](https://github.com/QaidVoid/errand/commit/ace9ba71f51336aeeb26543644f1ec2ffa553efd))
- (sandbox) Close the daemon's symlink-following sinks ([088ee6a](https://github.com/QaidVoid/errand/commit/088ee6a0c9235633f203ead52d6a3cbe5e9ea5c1))
- (sandbox) Point bailey user dirs at the operator home ([5f4ea77](https://github.com/QaidVoid/errand/commit/5f4ea7720bc67211a2b33bde2eb8bf96dfa47f31))
- (session) Refuse symlinks in recall and delegation answers ([6c4cafa](https://github.com/QaidVoid/errand/commit/6c4cafab783ee0f167652e3257ed88997c248721))

## [0.2.3] - 2026-09-29


### Features
- (model) List models in the channel, and answer slash commands ([c436fd2](https://github.com/QaidVoid/errand/commit/c436fd2db197da42df3c9490b0b06babe5da9a44))
- (sandbox) Grant execute on /workspace by default ([7a0e1f1](https://github.com/QaidVoid/errand/commit/7a0e1f13494a61dc5fdf4f8ded71f509d066f031))
- (usage) Let an operator re-read the usage window ([133fb67](https://github.com/QaidVoid/errand/commit/133fb6747763007a217780d893d2825a8e81d736))

### Fixes
- (model) Let a short name added later reach a running session ([ce59a96](https://github.com/QaidVoid/errand/commit/ce59a9663a7ba64990c050d23a8e9819dbe1e58b))
- (session) Resume on the configured model when a provider is gone ([8f117c6](https://github.com/QaidVoid/errand/commit/8f117c6360227a604be8c11fb8e28362ae0f16d4))

## [0.2.2] - 2026-09-24


### Features
- (chat) Drop the bars from the !usage table ([3051c91](https://github.com/QaidVoid/errand/commit/3051c9148a4b82bf22f968707c8260d3045ef271))
- (chat) Draw !usage as a bordered table with usage bars ([a46bf13](https://github.com/QaidVoid/errand/commit/a46bf13d7027e61a9b7e94eee577ff86c221186c))
- (chat) Show !usage as a table with reset times in UTC ([388f2ad](https://github.com/QaidVoid/errand/commit/388f2ad01408dd8c845dd26250ae357272f03674))
- (issues) Listen only in chosen repositories, for chosen triggers ([e772f7e](https://github.com/QaidVoid/errand/commit/e772f7eb55acc0413b5277da8c75de8c76b39070))
- (issues) Run GitHub sessions in a chat thread beside the issue ([24e6ade](https://github.com/QaidVoid/errand/commit/24e6ade6d99b8804c1519f215554d773ee3dc912))
- (issues) Start sessions from GitHub mentions and assignments ([c1f4b46](https://github.com/QaidVoid/errand/commit/c1f4b46325a51ae677c16eb85637a64e2347f8f9))
- (log) Add -v, -vv and -vvv for debug, trace and wire logs ([51642b7](https://github.com/QaidVoid/errand/commit/51642b7b10bba2c1a1a5fcadf845f2ad03932d81))
- (provider) Read several usage windows, a spent one first ([941ddac](https://github.com/QaidVoid/errand/commit/941ddac2b80a83e96fe4d1d03e5c7fcea6fb7022))
- (provider) Read usage by declared shape, or where a mapping says ([6a300f5](https://github.com/QaidVoid/errand/commit/6a300f5c49ba5edfdb91dbbc822aa1de0bbed099))
- (provider) Ask providers for their models, with per-model overrides ([01d0e64](https://github.com/QaidVoid/errand/commit/01d0e64f0b09e48becdaaee4132ab4ddb4c2028b))
- (sandbox) Forward plain http on the allowed egress ports ([ffe4c72](https://github.com/QaidVoid/errand/commit/ffe4c72d6646adceed3a4f8b7c0404e0791d07db))
- (session) Start on a fallback model when a window is spent ([e623a16](https://github.com/QaidVoid/errand/commit/e623a16193c01c2cc8476f8fb2e446b041e4cca8))
- (session) Let !pr name the repository with --repo ([e21a540](https://github.com/QaidVoid/errand/commit/e21a5401ffef65512d8fb43ceaf336f881fca92c))
- (session) Tell the agent what came of its pull request ([c2e8a9e](https://github.com/QaidVoid/errand/commit/c2e8a9e99dfd23cc4fb5f5ba80e4749a91a21d30))
- (session) Suggest diskTmp when a session runs out of scratch ([1f1b02b](https://github.com/QaidVoid/errand/commit/1f1b02b7d2f807011fa360a08973ddcb0d036566))

### Fixes
- (agent) Forward the provider's error to chat and the log ([678b0ea](https://github.com/QaidVoid/errand/commit/678b0ea5c3872316e41e4f243101299e09e908b5))
- (chat) Write a GitHub login bare where it cannot be mentioned ([61a1b61](https://github.com/QaidVoid/errand/commit/61a1b61ed550ee344d971b23f7bf20d2d845e6e6))
- (issues) Post only what a turn came to on the issue ([1262828](https://github.com/QaidVoid/errand/commit/126282840a9ede72b3bfab5e6dfc67e53edcb30d))
- (issues) Skip notifications with nothing new since the start ([da04669](https://github.com/QaidVoid/errand/commit/da046695b46821de0039b6e8a7ec4618f46957e1))
- (sandbox) Tell a name that does not resolve from an internal one ([5d61aaf](https://github.com/QaidVoid/errand/commit/5d61aafbbe61bce145b139a5fa03bbe87d693b61))
- (sandbox) Give podman sessions the providers and extensions ([6956e65](https://github.com/QaidVoid/errand/commit/6956e6553e30d7db08510ff84a3ab140edfe92ee))
- (sandbox) Keep every provider's real key out of a brokered session ([c9144c7](https://github.com/QaidVoid/errand/commit/c9144c7ae45ba3528a6f0050f7575acaeda6be48))
- (sandbox) Give every provider its key when egress is not brokered ([937468a](https://github.com/QaidVoid/errand/commit/937468ace316d51d185d10e3afbb066839a39048))
- (sandbox) Lay model entries over the agent's built-in definitions ([07b1269](https://github.com/QaidVoid/errand/commit/07b1269fa1c4f95ecd38d7b156ddd7e294cf9114))
- (sandbox) Honor tmpSize, shmSize and diskTmp under podman ([7979aad](https://github.com/QaidVoid/errand/commit/7979aadb080dc4b4c4a56f2148cd09dc74b72b0d))
- (sandbox) Start each launch on an empty disk-backed /tmp ([971192b](https://github.com/QaidVoid/errand/commit/971192b3e729144d6070dadef7a1f76e47b8885a))
- (sandbox) Bound broker lookups and dials ([6ec654a](https://github.com/QaidVoid/errand/commit/6ec654af1ca7ca244e692f5f2874b0af6d7e8620))
- (serve) Meter every z.ai provider, not only the default ([88b8627](https://github.com/QaidVoid/errand/commit/88b86277b4c33b5dd5cfab6532cb2d632978d860))
- (serve) Broker built-in providers from the host's model store ([55177d0](https://github.com/QaidVoid/errand/commit/55177d050aca775c995706ae1b55ce09bac36cc3))
- (session) Name a repository by its directory, not its GitHub name ([9f9031e](https://github.com/QaidVoid/errand/commit/9f9031eaab3d30fc3eaf5196b0dc3e654532bba6))
- (session) Find a repository cloned below the top of the session ([8874abe](https://github.com/QaidVoid/errand/commit/8874abe36bd4cc6832967eefc8b26e398e356e95))
- (session) Announce a model switch only once the agent takes it ([075576e](https://github.com/QaidVoid/errand/commit/075576edcf36f1d9cbf6b5546d6929934b143125))

## [0.2.1] - 2026-09-21


### Features
- (agent) Load pi extensions in the sandbox, and providers they register ([816bbeb](https://github.com/QaidVoid/errand/commit/816bbeb1287276f9996ce37b3138cb38bd2e1978))
- (memory) Let the agent recall older facts on demand ([a69829f](https://github.com/QaidVoid/errand/commit/a69829fba204c4be746b7d77d6d54793ffd5c36e))
- (sandbox) Let a session's /tmp be backed by disk ([72dd7ab](https://github.com/QaidVoid/errand/commit/72dd7ab5d16874af0962b2dcaf96cbc8bdd6e871))
- (sandbox) Make the private /tmp and /dev/shm sizes configurable ([011697b](https://github.com/QaidVoid/errand/commit/011697b84e93ac76edb7d4e481db740e791324eb))
- (session) Say a resource-limited session can be continued ([e290a1c](https://github.com/QaidVoid/errand/commit/e290a1ca24abb0bebcf18429343de961111c0be2))

### Fixes
- (memory) Tell the agent its recall is in the prompt, not the files ([979c221](https://github.com/QaidVoid/errand/commit/979c221eed389a7a034a5716638529b549202181))
- (session) Start a bare model on the provider that serves it ([65c094b](https://github.com/QaidVoid/errand/commit/65c094b0d982a73d4d071e657e2da9eba2dfb515))
- (session) Name the sandbox scratch limits on ENOSPC ([0a77725](https://github.com/QaidVoid/errand/commit/0a77725d39e2f81c4644e6476e1dd13955d33baa))
- (session) Retry a dropped attachment fetch and say why it failed ([10766eb](https://github.com/QaidVoid/errand/commit/10766eb9011300edefcff2772822fee1c7b5bd9f))

## [0.2.0] - 2026-09-18


### Features
- (chat) Name the model on the opening line and every done ([29ca800](https://github.com/QaidVoid/errand/commit/29ca800335790d6876174d31ab0bcb9d2bd3b8d4))
- (chat) Group what a turn took and what it spent ([e5ffd5d](https://github.com/QaidVoid/errand/commit/e5ffd5d5f9a0340a3fe39747af63d5a5ef55678c))
- (config) [breaking] Describe every provider in one place ([d87057a](https://github.com/QaidVoid/errand/commit/d87057adea9fbf83c96b4ed5d8dc13672be7a031))
- (provider) Let a model say how hard it thinks ([cf8f505](https://github.com/QaidVoid/errand/commit/cf8f505bb340a3818ed094216808c457ba15160b))
- (session) Make the model listing something to type back ([c0e31a9](https://github.com/QaidVoid/errand/commit/c0e31a91800009f79beaf9f3a39111e53178d520))
- (session) Say how long a turn waited and how long it took ([6c6459e](https://github.com/QaidVoid/errand/commit/6c6459eef74adb3eac08ea78627ceda8d5b8d7dd))

### Fixes
- (config) Let the configured model name its provider ([5700e87](https://github.com/QaidVoid/errand/commit/5700e877b6d488a66aaeb77f626cd52ffe67c4db))
- (config) Report the search that actually happened ([990d6c5](https://github.com/QaidVoid/errand/commit/990d6c56c0101409e8e76a407bcb1a8f415c1fa3))
- (daemon) Answer where the question was asked, not the channel ([4d9da77](https://github.com/QaidVoid/errand/commit/4d9da77dc296ca6c9dcd5735a1e111375d0a8f84))
- (pr) Open a pull request on the bot's own repository ([a447f6f](https://github.com/QaidVoid/errand/commit/a447f6fd88a593ce8216da4cbbfa1a0f79568d73))
- (provider) Skip a malformed model store entry, not the daemon ([d6c8f89](https://github.com/QaidVoid/errand/commit/d6c8f8948fcf42ce08901545122976bece048300))
- (session) Report the model a switch set, not the last turn's ([01fef5d](https://github.com/QaidVoid/errand/commit/01fef5da3c2e0d6dabc552aa96a7c686cb8d7dc1))
- (session) Say a thinking level apart from the model name ([15cfb74](https://github.com/QaidVoid/errand/commit/15cfb74277c4926923fe3c79c97c9adee0589603))
- (session) Match an aliased model without its level ([129251b](https://github.com/QaidVoid/errand/commit/129251b379ac4d0d166403f149cc0cce5564a3fc))
- (session) Let a switch name a model on any configured provider ([c280f8d](https://github.com/QaidVoid/errand/commit/c280f8d29c59e0e483279d86adf643258339bfd2))

### Build
- Let vitepress put the site where cloudflare looks ([e86c7b3](https://github.com/QaidVoid/errand/commit/e86c7b3389ed6dce6037214dc3b58e74e65e0d6c))
