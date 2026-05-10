import Link from "next/link";
import type { ComponentProps } from "react";
import { MDXRemote } from "next-mdx-remote/rsc";

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

export function BlogMDX({ source }: { source: string }) {
  return <MDXRemote source={source} components={components} />;
}
