<script setup lang="ts">
/**
 * The whole page.
 *
 * Written rather than inherited: the default theme is recognisable at a
 * glance, and a tool whose own interface has a considered look should not
 * document itself in somebody else's. What is here is what a reference needs
 * and no more: where you are, what else there is, and the page.
 */
import { computed, onMounted, ref, watch } from "vue";
import { useData, useRoute, useRouter } from "vitepress";

const { site, page, frontmatter, theme } = useData();
const route = useRoute();
const router = useRouter();

/** The sidebar, flattened from the configured groups. */
const groups = computed(() => theme.value.sidebar ?? []);

/** Every page in order, so one can be followed by the next. */
const ordered = computed(() =>
  groups.value.flatMap((group: { items: { text: string; link: string }[] }) => group.items)
);

const here = computed(() => route.path.replace(/index\.html$/, "").replace(/\.html$/, ""));

function current(link: string): boolean {
  const path = link.replace(/\/$/, "");
  return here.value.replace(/\/$/, "") === path;
}

const at = computed(() => ordered.value.findIndex((item) => current(item.link)));
const previous = computed(() => (at.value > 0 ? ordered.value[at.value - 1] : undefined));
const next = computed(() =>
  at.value >= 0 && at.value < ordered.value.length - 1 ? ordered.value[at.value + 1] : undefined
);

/** The page's own headings, for moving inside a long reference. */
const outline = ref<{ text: string; id: string; depth: number }[]>([]);

function readOutline(): void {
  const found = [...document.querySelectorAll(".body h2, .body h3")];
  outline.value = found.map((heading) => ({
    text: heading.textContent?.replace(/\u200B/g, "").trim() ?? "",
    id: heading.id,
    depth: heading.tagName === "H2" ? 2 : 3,
  }));
}

onMounted(() => {
  readOutline();
  restore();
});
watch(() => route.path, () => setTimeout(readOutline, 0));

/** Which theme is shown. The interface remembers the same way. */
const chosen = ref<"light" | "dark" | "system">("system");

function apply(choice: "light" | "dark" | "system"): void {
  chosen.value = choice;
  if (choice === "system") {
    localStorage.removeItem("theme");
    document.documentElement.removeAttribute("data-theme");
    return;
  }
  localStorage.setItem("theme", choice);
  document.documentElement.setAttribute("data-theme", choice);
}

function restore(): void {
  const held = localStorage.getItem("theme");
  apply(held === "light" || held === "dark" ? held : "system");
}

/** Open on a narrow screen, where the sidebar is a drawer. */
const drawer = ref(false);
watch(() => route.path, () => (drawer.value = false));
</script>

<template>
  <div class="shell" :data-drawer="drawer">
    <header class="top">
      <button class="pull" @click="drawer = !drawer" aria-label="Pages">pages</button>
      <a class="wordmark" href="/">errand</a>
      <span class="tag">{{ site.description }}</span>
      <div class="theme" role="group" aria-label="Theme">
        <button
          v-for="choice in (['light', 'system', 'dark'] as const)"
          :key="choice"
          :class="{ on: chosen === choice }"
          :aria-pressed="chosen === choice"
          @click="apply(choice)"
        >{{ choice === "system" ? "auto" : choice }}</button>
      </div>
      <a class="repo" :href="theme.repo" rel="noreferrer">source</a>
    </header>

    <div class="page">
      <nav class="pages" aria-label="Pages">
        <template v-for="group in groups" :key="group.text">
          <p class="group">{{ group.text }}</p>
          <a
            v-for="item in group.items"
            :key="item.link"
            :href="item.link"
            :class="{ on: current(item.link) }"
          >{{ item.text }}</a>
        </template>
      </nav>

      <main>
        <article class="body">
          <Content />
        </article>

        <nav class="along" v-if="previous || next">
          <a v-if="previous" :href="previous.link" class="back">
            <span>before</span>{{ previous.text }}
          </a>
          <a v-if="next" :href="next.link" class="on-to">
            <span>next</span>{{ next.text }}
          </a>
        </nav>
      </main>

      <nav class="outline" aria-label="On this page" v-if="outline.length > 1">
        <p class="group">on this page</p>
        <a
          v-for="heading in outline"
          :key="heading.id"
          :href="`#${heading.id}`"
          :class="{ deep: heading.depth === 3 }"
        >{{ heading.text }}</a>
      </nav>
    </div>
  </div>
</template>
