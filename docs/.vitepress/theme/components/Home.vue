<script setup lang="ts">
import { withBase } from "vitepress";

const features = [
  {
    title: "Sandboxed, always",
    body: "Every session runs under bailey, rootless podman, or Incus. There is no unsandboxed mode, and a guarantee the host cannot keep stops the daemon.",
    link: "/operations/sandboxing",
  },
  {
    title: "One thread per session",
    body: "A message opens a thread. Replies are prompts, a reply mid-turn redirects it, and the thread survives restarts so the session can be resumed.",
    link: "/guide/threads",
  },
  {
    title: "Brokered egress",
    body: "Each session gets its own proxy. It holds a stand-in for the provider key, reaches only the hosts you allow, and every provider call leaves a receipt.",
    link: "/operations/egress",
  },
  {
    title: "Switch models mid-conversation",
    body: "Plan on a capable model, switch to a cheaper one to carry it out, and switch back to review. Usage windows and fallbacks are built in.",
    link: "/guide/models",
  },
  {
    title: "Issues become sessions",
    body: "Mention or assign the bot on GitHub or Forgejo. The work happens in a thread you can steer, and each turn's answer lands on the issue.",
    link: "/guide/forges",
  },
  {
    title: "A web view of every session",
    body: "Read a session as it happens, browse its project, and start new ones from a browser, with Discord login when you publish it.",
    link: "/guide/interface",
  },
];

const steps = [
  { n: "01", title: "Ask", body: "Post in the served channel, or mention the bot on an issue." },
  { n: "02", title: "Confine", body: "A sandbox starts with one project directory and nothing else." },
  { n: "03", title: "Steer", body: "Watch tool calls and diffs stream in. Reply to redirect the turn." },
  { n: "04", title: "Ship", body: "Ask for a pull request. The daemon composes and pushes it." },
];

const backends = [
  {
    name: "bailey",
    body: "A confined host process with Landlock, seccomp, and namespaces. Uses the host's own tools, no image needed.",
  },
  {
    name: "podman",
    body: "A rootless container from an image you build, reaching only its broker over loopback.",
  },
  {
    name: "incus",
    body: "An unprivileged system container with its own init, so Docker, databases, and FUSE work inside.",
  },
];
</script>

<template>
  <div class="home">
    <section class="hero">
      <div class="hero-copy">
        <p class="eyebrow"><span class="dot" />a coding agent, driven from chat</p>
        <h1>
          Send an agent
          <br />
          on an <span class="em">errand</span>
        </h1>
        <p class="lede">
          Post in a channel. errand opens a thread, starts a sandboxed session in a project of its
          own, and streams back what the agent says, runs, and changes. Reply to steer it.
        </p>
        <div class="actions">
          <a class="btn primary" :href="withBase('/guide/quick-start')">Get started</a>
          <a class="btn" :href="withBase('/guide/introduction')">How it works</a>
        </div>
        <div class="install">
          <span class="prompt">$</span>
          <code>errand run</code>
        </div>
      </div>

      <div class="thread" aria-label="An example session in a chat thread">
        <div class="thread-head">
          <span class="hash">#</span>agents
          <span class="arrow" />
          <span class="thread-name">the parser drops the last line</span>
        </div>

        <div class="msg">
          <div class="avatar you">Y</div>
          <div class="msg-body">
            <p class="who">you <span class="state ok">done</span></p>
            <p>the parser drops the last line of a file with no trailing newline. fix it</p>
          </div>
        </div>

        <div class="msg">
          <div class="avatar bot" />
          <div class="msg-body">
            <p class="who">errand <span class="tag">bot</span></p>
            <div class="tools">
              <p><span class="ran">ran</span> <code>rg -n "fn parse" src</code></p>
              <p><span class="ran">ran</span> <code>cargo test parser</code></p>
              <p><span class="ran">edited</span> <code>src/parse.rs</code></p>
            </div>
            <pre class="diff"><span class="rm">-    for line in text.split_terminator('\n') {</span>
<span class="add">+    for line in text.lines() {</span></pre>
            <p>
              Fixed. <code>split_terminator</code> skipped a final unterminated line, and the new
              test covers it.
            </p>
            <p class="receipt"><code>glm-5.3</code> | 41s (1.2s to first word) | 18.4k tokens, 62% cached</p>
          </div>
        </div>

        <div class="msg">
          <div class="avatar you">Y</div>
          <div class="msg-body">
            <p class="who">you <span class="state run">running</span></p>
            <p><code>!pr fix the trailing line in the parser</code></p>
          </div>
        </div>
      </div>
    </section>

    <section class="band">
      <div class="section-head">
        <p class="kicker">what you get</p>
        <h2>Built for running an agent you do not fully trust</h2>
      </div>
      <div class="features">
        <a v-for="f in features" :key="f.title" class="feature" :href="withBase(f.link)">
          <h3>{{ f.title }}</h3>
          <p>{{ f.body }}</p>
        </a>
      </div>
    </section>

    <section class="band">
      <div class="section-head">
        <p class="kicker">the loop</p>
        <h2>From a message to a pull request</h2>
      </div>
      <ol class="steps">
        <li v-for="s in steps" :key="s.n">
          <span class="n">{{ s.n }}</span>
          <h3>{{ s.title }}</h3>
          <p>{{ s.body }}</p>
        </li>
      </ol>
    </section>

    <section class="band">
      <div class="section-head">
        <p class="kicker">three backends</p>
        <h2>Pick how a session is confined</h2>
      </div>
      <div class="backends">
        <div v-for="b in backends" :key="b.name" class="backend">
          <p class="name"><span class="caret" />{{ b.name }}</p>
          <p>{{ b.body }}</p>
        </div>
      </div>
    </section>

    <section class="cta">
      <h2>One binary, one channel, as many sessions as you need.</h2>
      <div class="actions">
        <a class="btn primary" :href="withBase('/guide/quick-start')">Install errand</a>
        <a class="btn" :href="withBase('/reference/configuration')">Configuration reference</a>
      </div>
    </section>
  </div>
</template>

<style scoped>
.home {
  --gap: clamp(20px, 4vw, 48px);
  max-width: 1240px;
  margin: 0 auto;
  padding: 0 var(--gap) 96px;
}

.hero {
  position: relative;
  display: grid;
  grid-template-columns: minmax(0, 1.05fr) minmax(0, 1fr);
  gap: clamp(32px, 5vw, 72px);
  align-items: center;
  padding: clamp(48px, 9vw, 112px) 0 clamp(48px, 7vw, 88px);
}

.hero::before {
  content: "";
  position: absolute;
  inset: 0 calc(-1 * var(--gap));
  z-index: -1;
  background-image: linear-gradient(var(--er-grid) 1px, transparent 1px),
    linear-gradient(90deg, var(--er-grid) 1px, transparent 1px);
  background-size: 32px 32px;
  mask-image: radial-gradient(ellipse 80% 70% at 30% 40%, #000 30%, transparent 75%);
}

.eyebrow {
  display: inline-flex;
  align-items: center;
  gap: 10px;
  margin: 0 0 22px;
  padding: 6px 12px;
  border: 1px solid var(--vp-c-border);
  border-radius: 999px;
  font-family: var(--vp-font-family-mono);
  font-size: 12px;
  letter-spacing: 0.04em;
  color: var(--vp-c-text-2);
  background: var(--vp-c-bg-elv);
}

.dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: var(--er-caret);
  box-shadow: 0 0 0 3px rgba(126, 226, 184, 0.2);
}

h1 {
  margin: 0;
  font-family: var(--er-font-display);
  font-size: clamp(44px, 6.4vw, 78px);
  line-height: 1;
  font-weight: 700;
  letter-spacing: -0.045em;
  color: var(--vp-c-text-1);
}

.em {
  color: var(--vp-c-brand-1);
}


.lede {
  max-width: 34em;
  margin: 26px 0 0;
  font-size: 18px;
  line-height: 1.6;
  color: var(--vp-c-text-2);
}

.actions {
  display: flex;
  flex-wrap: wrap;
  gap: 12px;
  margin-top: 32px;
}

.btn {
  display: inline-flex;
  align-items: center;
  height: 44px;
  padding: 0 20px;
  border: 1px solid var(--vp-c-border);
  border-radius: 10px;
  font-weight: 600;
  font-size: 15px;
  color: var(--vp-c-text-1);
  background: var(--vp-c-bg-elv);
  text-decoration: none;
  transition: border-color 0.15s, transform 0.15s;
}

.btn:hover {
  border-color: var(--vp-c-brand-1);
  transform: translateY(-1px);
}

.btn.primary {
  border-color: transparent;
  color: var(--er-ink);
  background: var(--er-caret);
}

.btn.primary:hover {
  background: #98eac6;
}

.install {
  display: inline-flex;
  align-items: center;
  gap: 12px;
  margin-top: 28px;
  padding: 10px 16px;
  border-radius: 10px;
  font-family: var(--vp-font-family-mono);
  font-size: 14px;
  color: #dbe3ef;
  background: var(--er-well);
  border: 1px solid #1a212c;
}

.install code {
  background: none;
  padding: 0;
  color: inherit;
}

.prompt {
  color: var(--er-caret);
}

/* The thread mock is always dark: it is a picture of a chat client. */

.thread {
  border: 1px solid #222b38;
  border-radius: 16px;
  background: #121821;
  color: #dbe3ef;
  box-shadow: 0 30px 80px -30px rgba(5, 8, 12, 0.6), 0 0 0 6px rgba(126, 226, 184, 0.04);
  overflow: hidden;
  font-size: 14px;
}

.thread-head {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 13px 18px;
  border-bottom: 1px solid #1f2733;
  font-weight: 600;
  color: #a3afc1;
  background: #0e131a;
}

.hash {
  color: #5d6a7e;
  font-weight: 500;
}

.arrow {
  width: 6px;
  height: 6px;
  border-right: 1.5px solid #5d6a7e;
  border-bottom: 1.5px solid #5d6a7e;
  transform: rotate(-45deg);
  margin: 0 4px;
}

.thread-name {
  color: #e3e9f2;
}

.msg {
  display: flex;
  gap: 12px;
  padding: 14px 18px;
}

.msg + .msg {
  border-top: 1px solid #18202b;
}

.avatar {
  flex: none;
  width: 34px;
  height: 34px;
  border-radius: 50%;
  display: grid;
  place-items: center;
  font-weight: 700;
  font-size: 13px;
}

.avatar.you {
  background: #2a3443;
  color: #a3afc1;
}

.avatar.bot {
  border-radius: 10px;
  background: linear-gradient(#2b3444, #161b24);
  border: 1px solid #3d4a5e;
  position: relative;
}

.avatar.bot::before {
  content: "";
  position: absolute;
  left: 8px;
  top: 11px;
  width: 7px;
  height: 7px;
  border-right: 2.5px solid var(--er-caret);
  border-bottom: 2.5px solid var(--er-caret);
  transform: rotate(-45deg);
}

.avatar.bot::after {
  content: "";
  position: absolute;
  right: 7px;
  bottom: 10px;
  width: 9px;
  height: 3px;
  border-radius: 2px;
  background: var(--er-reply);
}

.msg-body {
  min-width: 0;
  flex: 1;
}

.msg-body p {
  margin: 0;
  line-height: 1.55;
}

.msg-body p + p {
  margin-top: 6px;
}

.who {
  display: flex;
  align-items: center;
  gap: 8px;
  font-weight: 600;
  color: #e3e9f2;
}

.tag {
  padding: 1px 5px;
  border-radius: 4px;
  font-size: 10px;
  font-weight: 700;
  text-transform: uppercase;
  background: var(--er-reply-deep);
  color: #fff;
}

.state {
  padding: 1px 8px;
  border-radius: 999px;
  font-family: var(--vp-font-family-mono);
  font-size: 11px;
  font-weight: 500;
}

.state.ok {
  color: var(--er-caret);
  background: rgba(126, 226, 184, 0.12);
}

.state.run {
  color: #f5c877;
  background: rgba(245, 200, 119, 0.12);
}

.msg-body code {
  font-family: var(--vp-font-family-mono);
  font-size: 12.5px;
  padding: 1px 5px;
  border-radius: 4px;
  background: rgba(219, 227, 239, 0.08);
  color: #e3e9f2;
}

.tools {
  margin: 4px 0 8px;
  padding: 8px 12px;
  border-left: 2px solid #2a3443;
  font-size: 13px;
  color: #a3afc1;
}

.tools p + p {
  margin-top: 3px;
}

.ran {
  font-family: var(--vp-font-family-mono);
  font-size: 11px;
  text-transform: uppercase;
  letter-spacing: 0.06em;
  color: #737f93;
}

.diff {
  margin: 0 0 8px;
  padding: 10px 12px;
  border-radius: 8px;
  background: #0a0d12;
  font-family: var(--vp-font-family-mono);
  font-size: 12px;
  line-height: 1.6;
  overflow-x: auto;
  white-space: pre;
}

.rm {
  color: #ff8b8b;
}

.add {
  color: var(--er-caret);
}

.receipt {
  font-family: var(--vp-font-family-mono);
  font-size: 11.5px;
  color: #737f93;
}

/* Sections */

.band {
  padding-top: clamp(56px, 8vw, 96px);
}

.section-head {
  margin-bottom: 32px;
}

.kicker {
  margin: 0 0 10px;
  font-family: var(--vp-font-family-mono);
  font-size: 12px;
  text-transform: uppercase;
  letter-spacing: 0.12em;
  color: var(--vp-c-brand-1);
}

.section-head h2,
.cta h2 {
  margin: 0;
  font-family: var(--er-font-display);
  font-size: clamp(28px, 3.4vw, 40px);
  line-height: 1.1;
  font-weight: 700;
  letter-spacing: -0.03em;
  color: var(--vp-c-text-1);
}

.features {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 16px;
}

.feature {
  display: block;
  padding: 24px;
  border: 1px solid var(--vp-c-divider);
  border-radius: 14px;
  background: var(--vp-c-bg-elv);
  text-decoration: none;
  color: inherit;
  transition: border-color 0.15s, transform 0.15s;
}

.feature:hover {
  border-color: var(--vp-c-brand-1);
  transform: translateY(-2px);
}

.feature h3 {
  margin: 0 0 8px;
  font-family: var(--er-font-display);
  font-size: 18px;
  font-weight: 650;
  letter-spacing: -0.01em;
  color: var(--vp-c-text-1);
}

.feature p,
.steps p,
.backend p {
  margin: 0;
  font-size: 14.5px;
  line-height: 1.6;
  color: var(--vp-c-text-2);
}

.steps {
  list-style: none;
  margin: 0;
  padding: 0;
  display: grid;
  grid-template-columns: repeat(4, minmax(0, 1fr));
  gap: 0;
  border-top: 1px solid var(--vp-c-divider);
}

.steps li {
  padding: 24px 24px 0 0;
}

.steps li + li {
  padding-left: 24px;
  border-left: 1px solid var(--vp-c-divider);
}

.n {
  font-family: var(--vp-font-family-mono);
  font-size: 13px;
  color: var(--vp-c-brand-1);
}

.steps h3 {
  margin: 10px 0 6px;
  font-family: var(--er-font-display);
  font-size: 22px;
  font-weight: 650;
  color: var(--vp-c-text-1);
}

.backends {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 16px;
}

.backend {
  padding: 24px;
  border-radius: 14px;
  background: var(--er-well);
  border: 1px solid #1a212c;
}

.backend .name {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-bottom: 10px;
  font-family: var(--vp-font-family-mono);
  font-size: 17px;
  font-weight: 600;
  color: #e3e9f2;
}

.backend p:not(.name) {
  color: #a3afc1;
}

.caret {
  width: 8px;
  height: 8px;
  border-right: 2.5px solid var(--er-caret);
  border-bottom: 2.5px solid var(--er-caret);
  transform: rotate(-45deg);
}

.cta {
  margin-top: clamp(64px, 9vw, 112px);
  padding: clamp(32px, 5vw, 56px);
  border-radius: 20px;
  border: 1px solid var(--vp-c-divider);
  background: radial-gradient(ellipse at top left, var(--vp-c-brand-soft), transparent 60%),
    var(--vp-c-bg-elv);
}

.cta h2 {
  max-width: 18em;
}

@media (max-width: 960px) {
  .hero {
    grid-template-columns: 1fr;
  }

  .features,
  .backends {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }

  .steps {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }

  .steps li:nth-child(3) {
    padding-left: 0;
    border-left: none;
  }
}

@media (max-width: 640px) {
  .features,
  .backends,
  .steps {
    grid-template-columns: 1fr;
  }

  .steps li,
  .steps li + li {
    padding-left: 0;
    border-left: none;
  }
}

</style>
