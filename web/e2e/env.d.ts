// The Playwright config and specs run under Node, but this project's tsconfig
// targets the browser. Declare only the environment access those files need,
// rather than pulling all of @types/node into the browser sources.
declare const process: {
  readonly env: Readonly<Record<string, string | undefined>>;
};
