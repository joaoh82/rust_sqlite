import Link from "next/link";
import type { ComponentProps } from "react";
import { MDXRemote } from "next-mdx-remote/rsc";
import rehypePrettyCode, {
  type Options as RehypePrettyCodeOptions,
  type Theme as RehypePrettyCodeTheme,
} from "rehype-pretty-code";
import { createCssVariablesTheme } from "shiki";

function isInternal(href: string | undefined): boolean {
  if (!href) return false;
  return href.startsWith("/") || href.startsWith("#");
}

function Anchor({ href, children, ...rest }: ComponentProps<"a">) {
  if (isInternal(href)) {
    return (
      <Link href={href ?? "#"} {...rest}>
        {children}
      </Link>
    );
  }
  return (
    <a href={href} target="_blank" rel="noreferrer" {...rest}>
      {children}
    </a>
  );
}

const components = {
  a: Anchor,
};

// Shiki emits inline styles like `style="color: var(--shiki-token-keyword)"`.
// `globals.css` maps those CSS vars onto the blog's existing color tokens, so
// the highlighter stays a pure data layer and themes stay coherent.
// `createCssVariablesTheme` returns `ThemeRegistration` (all-optional fields);
// rehype-pretty-code wants `ThemeRegistrationRaw` (settings required). The
// shapes are structurally compatible — shiki tolerates the missing `settings`
// because the rules live in `tokenColors`.
const shikiTheme = createCssVariablesTheme({
  name: "sqlrite-css-vars",
}) as RehypePrettyCodeTheme;

const prettyCodeOptions: RehypePrettyCodeOptions = {
  theme: shikiTheme,
  keepBackground: false,
  // Inline `code` keeps the existing `.blog-article-body code:not(pre code)`
  // chip style — only fenced blocks get tokenized.
  bypassInlineCode: true,
  // Fences without a language tag (or with an unknown one) fall back to
  // plaintext rather than throwing during the build.
  defaultLang: { block: "plaintext" },
};

export function BlogMDX({ source }: { source: string }) {
  return (
    <MDXRemote
      source={source}
      components={components}
      options={{
        mdxOptions: {
          rehypePlugins: [[rehypePrettyCode, prettyCodeOptions]],
        },
      }}
    />
  );
}
