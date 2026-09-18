# Changelog

What changed in each release, from the commit messages.
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
