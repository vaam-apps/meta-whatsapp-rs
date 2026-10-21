import { statSync } from "node:fs";
import { posix } from "node:path";
import { fileURLToPath } from "node:url";
import { defineMd3Config } from "@vaam-apps/vitepress-md3/config";

// Served from GitHub Pages at /meta-whatsapp-rs/. The docs workflow passes
// the path Pages reports in DOCS_BASE (empty on pull requests, hence `||`).
const base = process.env.DOCS_BASE || "/meta-whatsapp-rs/";
const repo = "https://github.com/vaam-apps/meta-whatsapp-rs";
const repoRoot = fileURLToPath(new URL("../..", import.meta.url));

/**
 * The pages are written to be read on GitHub too, so they link to files
 * outside docs/ (`../../crates/…`, `../../OPEN_QUESTIONS.md`) with relative
 * paths, which a test in the workspace checks. On the site those would be
 * dead links: point them at the file on GitHub instead.
 */
function outsideDocs(href: string, page: string): string | undefined {
  if (/^[a-z][a-z0-9+.-]*:|^[/#]/i.test(href)) return undefined;
  const [path, hash] = href.split("#", 2);
  const inDocs = posix.normalize(posix.join(posix.dirname(page), path));
  if (!inDocs.startsWith("../")) return undefined;
  const inRepo = inDocs.slice("../".length);
  let kind = "blob";
  try {
    if (statSync(`${repoRoot}/${inRepo}`).isDirectory()) kind = "tree";
  } catch {
    // Missing: let it through as a file link; the workspace test reports it.
  }
  return `${repo}/${kind}/main/${inRepo.replace(/\/$/, "")}${hash ? `#${hash}` : ""}`;
}

/** GitHub's heading anchors, which the pages' own links are written for. */
function githubSlug(heading: string): string {
  return heading
    .trim()
    .toLowerCase()
    .replace(/[^\p{L}\p{N}\s_-]/gu, "")
    .replace(/\s/g, "-");
}

export default defineMd3Config({
  base,
  lang: "en",
  title: "meta-whatsapp-rs",
  titleTemplate: ":title · meta-whatsapp-rs",
  description:
    "WhatsApp Business Platform for Rust: typed Cloud API client, webhooks, Embedded Signup, templates, OTP, adapters, Typst documents.",
  cleanUrls: true,
  lastUpdated: true,
  // A link to a page that does not exist fails the build.
  ignoreDeadLinks: false,
  head: [["link", { rel: "icon", type: "image/svg+xml", href: `${base}logo.svg` }]],

  markdown: {
    anchor: { slugify: githubSlug },
    config(md) {
      md.core.ruler.push("links_outside_docs", (state) => {
        const page: string = state.env?.relativePath ?? "";
        for (const block of state.tokens) {
          for (const token of block.children ?? []) {
            if (token.type !== "link_open") continue;
            const href = token.attrGet("href");
            const rewritten = href && outsideDocs(href, page);
            if (rewritten) token.attrSet("href", rewritten);
          }
        }
      });
    },
  },

  themeConfig: {
    md3: {
      style: "expressive",
      seed: "#128c7e",
    },

    logo: "/logo.svg",

    nav: [
      { text: "Get started", link: "/getting-started" },
      { text: "Guides", link: "/guides/README", activeMatch: "^/guides/" },
      { text: "Architecture", link: "/architecture", activeMatch: "^/(architecture|design/)" },
      { text: "Status", link: "/coverage", activeMatch: "^/(coverage|parity|categories)" },
      {
        text: "More",
        items: [
          { text: "Roadmap", link: "/roadmap" },
          { text: "Development environment", link: "/dev-environment" },
          { text: "Contributing", link: `${repo}/blob/main/CONTRIBUTING.md` },
          { text: "Changelog", link: `${repo}/blob/main/CHANGELOG.md` },
          { text: "All vaam-apps docs", link: "https://vaam-apps.github.io/vitepress-md3/projects/" },
        ],
      },
    ],

    sidebar: [
      {
        text: "Start here",
        items: [
          { text: "Introduction", link: "/" },
          { text: "Getting started", link: "/getting-started" },
        ],
      },
      {
        text: "Guides",
        items: [
          { text: "Which guide for which job", link: "/guides/README" },
          { text: "Meta setup and first message", link: "/guides/getting-started" },
          { text: "Embedded Signup", link: "/guides/embedded-signup" },
          { text: "Webhooks", link: "/guides/webhooks" },
          { text: "CMS inbox", link: "/guides/cms-inbox" },
          { text: "Marketing and commerce", link: "/guides/marketing-and-commerce" },
          { text: "WhatsApp OTP login", link: "/guides/otp-login" },
          { text: "Documents", link: "/guides/documents" },
          { text: "Bots", link: "/guides/bots" },
          { text: "Production", link: "/guides/production" },
          { text: "Run the service", link: "/guides/server" },
        ],
      },
      {
        text: "Architecture and design",
        items: [
          { text: "Architecture", link: "/architecture" },
          { text: "Design: the service", link: "/design/server" },
        ],
      },
      {
        text: "What is implemented",
        items: [
          { text: "Feature coverage", link: "/coverage" },
          { text: "Parity", link: "/parity" },
          { text: "Meta platform categories", link: "/categories" },
        ],
      },
      {
        text: "Plan",
        items: [
          { text: "Roadmap to parity", link: "/roadmap" },
          { text: "Open questions", link: `${repo}/blob/main/OPEN_QUESTIONS.md` },
        ],
      },
      {
        text: "Development",
        items: [
          { text: "Development environment", link: "/dev-environment" },
          { text: "Contributing", link: `${repo}/blob/main/CONTRIBUTING.md` },
          { text: "Changelog", link: `${repo}/blob/main/CHANGELOG.md` },
        ],
      },
    ],

    outline: { level: [2, 3] },
    search: { provider: "local" },
    socialLinks: [{ icon: "github", link: repo }],
    editLink: {
      pattern: `${repo}/edit/main/docs/:path`,
      text: "Edit this page on GitHub",
    },
    footer: {
      message: "Released under the MIT License.",
    },
  },
});
