# Changelog

What changed in each release, from the commit messages.
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
