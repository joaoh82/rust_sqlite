import {
  codeToHtml,
  createCssVariablesTheme,
  type ShikiTransformer,
} from "shiki";

// Shiki emits inline styles like `color: var(--shiki-token-keyword)`. The
// CSS-variable mapping onto the project's palette tokens lives in
// `globals.css` (`.blog-article-body pre`, `.code-body`, …) so colors stay
// theme-coherent and adjustable in one place.
export const sqlriteShikiTheme = createCssVariablesTheme({
  name: "sqlrite-css-vars",
});

// `createCssVariablesTheme` still sets `background: var(--shiki-background)`
// on the outer <pre>; that would force the figure to override our existing
// container backgrounds. Strip the inline style so the surrounding wrapper
// (.code-body, .blog-article-body pre, etc.) drives layout/colors.
const stripContainerStyle: ShikiTransformer = {
  pre(node) {
    if (node.properties) delete node.properties.style;
  },
};

export async function highlightCode(
  code: string,
  lang: string,
): Promise<string> {
  return codeToHtml(code, {
    lang,
    theme: sqlriteShikiTheme,
    transformers: [stripContainerStyle],
  });
}
