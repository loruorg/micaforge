/**
 * Finding analytics calls in JavaScript and TypeScript.
 *
 * The code is parsed with `@babel/parser` (JavaScript, TypeScript, JSX, with error
 * recovery) and walked twice. The first walk learns the file's names for things: which
 * local identifier is `track` from `@micaforge/sdk`, which variable holds a tracker from
 * `useMicaforge()` or `getTracker()`, which holds a `@micaforge/sdk-server` client. The
 * second finds the calls through those names, plus the forms that need no import: the
 * global `window.micaforge`, the queue `micaforge("event", …)`, a `data-track` attribute
 * in JSX, and `useMicaforge()`, matched by name because Nuxt imports composables for you.
 *
 * Binding is by name within a file, not by scope. A file that shadows `track` with an
 * unrelated function inside a nested scope is the price; following real scopes would
 * cost an analyser this tool does not need to be.
 *
 * A name is read when the source says it: a string, a template without holes, a `const`
 * holding one, a property of a `const` object (`EVENTS.SIGNUP`), either side of a
 * `?:`. Anything else is reported as computed, with the fixed parts kept as a glob when
 * there are some (`` `step_${n}` `` is `step_*`), so the comparison with the live site
 * can still match it.
 */
import { parse, parseExpression } from "@babel/parser";

import type { Occurrence, PropInfo, SourceKind } from "./model.js";
import { LineIndex, scanMarkup } from "./tags.js";

/** A Babel AST node, read structurally. */
export interface Node {
  type: string;
  start?: number | null;
  end?: number | null;
  loc?: { start: { line: number; column: number } } | null;
  [key: string]: any;
}

/** What one file has taught the scanner, and what it found in it. */
export interface FileContext {
  file: string;
  occurrences: Occurrence[];
  /** Problems that stopped a part of the file from being read. */
  errors: { line: number; message: string }[];
  /** Local names of track functions: `track`, a `useTrackEvent()` result, `{ track }`. */
  trackFns: Map<string, SourceKind>;
  /** Local names of tracker objects, with `.track(name, props)`. */
  trackerObjs: Map<string, SourceKind>;
  /** Functions that return a tracker object. */
  trackerFactories: Map<string, SourceKind>;
  /** Functions that return a track function. */
  trackFactories: Set<string>;
  /** Local names of `@micaforge/sdk-server` clients. */
  serverObjs: Set<string>;
  /** Local names of the `Micaforge` class. */
  serverClasses: Set<string>;
  /** Local names of namespace imports of `@micaforge/sdk-server`. */
  serverNamespaces: Set<string>;
  /** Functions whose result holds a `client` (`createInstrumentation`). */
  serverFactories: Set<string>;
  /** Components whose `event` prop is an event name (`TrackClick`). */
  components: Set<string>;
  /** Svelte actions whose argument is an event (`trackClick`). */
  actions: Set<string>;
  /** `const NAME = "literal"`. */
  constStrings: Map<string, string>;
  /** `const NAME = { ... }`. */
  constObjects: Map<string, Node>;
  /** The file imports `@micaforge/sdk-server`, so `x.track({ name })` is a server call. */
  importsServer: boolean;
  /** Names imported from the project's own modules: local name to module and export. */
  imported: Map<string, ModuleRef>;
  /** Call sites whose name is a constant from another module, for the scan to resolve. */
  refs: Map<Occurrence, ModuleRef>;
}

/**
 * An export of another module of the project: `CHECKOUT` from `./names`, or the `SIGNUP`
 * member of `EVENTS` from `./events`. `name` is `*` for a namespace import.
 */
export interface ModuleRef {
  source: string;
  name: string;
  member?: string;
}

/** A fresh context. The names matched without an import are set here. */
export function createContext(file: string): FileContext {
  return {
    file,
    occurrences: [],
    errors: [],
    trackFns: new Map(),
    trackerObjs: new Map(),
    // Nuxt imports composables for you, so `useMicaforge` is matched by its name.
    trackerFactories: new Map([["useMicaforge", "composable"]]),
    trackFactories: new Set(["useTrackEvent"]),
    serverObjs: new Set(),
    serverClasses: new Set(["Micaforge"]),
    serverNamespaces: new Set(),
    serverFactories: new Set(),
    components: new Set(["TrackClick"]),
    actions: new Set(["trackClick"]),
    constStrings: new Map(),
    constObjects: new Map(),
    importsServer: false,
    imported: new Map(),
    refs: new Map(),
  };
}

const SDK = /^@micaforge\/sdk(?:\/(?:react|vue|svelte|next))?$/;
const SERVER = /^@micaforge\/sdk-server(?:\/[\w-]+)?$/;
const NUXT = /^(?:@micaforge\/nuxt(?:\/[\w-]+)?|#imports|#app)$/;
const GLOBALS = new Set(["window", "globalThis", "self"]);

/** Where a parse starts in the file, for code embedded in a template. */
export interface Origin {
  line: number;
  /** 0-based column of the first line. */
  column: number;
}

/** Babel's plugins for a file type. */
function plugins(typescript: boolean, jsx: boolean): any[] {
  const list: any[] = ["decorators-legacy"];
  if (typescript) list.push("typescript");
  if (jsx) list.push("jsx");
  return list;
}

/** Parse a whole script. `null` when it cannot be read at all; the error is recorded. */
export function parseScript(
  ctx: FileContext,
  code: string,
  typescript: boolean,
  jsx: boolean,
  origin: Origin = { line: 1, column: 0 },
): Node | null {
  try {
    return parse(code, {
      sourceType: "unambiguous",
      errorRecovery: true,
      allowReturnOutsideFunction: true,
      allowAwaitOutsideFunction: true,
      allowImportExportEverywhere: true,
      allowUndeclaredExports: true,
      startLine: origin.line,
      startColumn: origin.column,
      plugins: plugins(typescript, jsx),
    }) as unknown as Node;
  } catch (error) {
    const line = (error as { loc?: { line?: number } }).loc?.line ?? origin.line;
    ctx.errors.push({ line, message: (error as Error).message.replace(/\s*\(\d+:\d+\)$/, "") });
    return null;
  }
}

/**
 * Parse the code in a template attribute: an expression, or failing that statements
 * (`@click="track('a'); open = true"`).
 *
 * `null`, and nothing recorded, when it is neither. Template languages put things in
 * attributes that are not JavaScript at all (`v-for="item of items"`), and a scan is not
 * the place to report them.
 */
export function parseFragment(code: string, origin: Origin, typescript: boolean): Node | null {
  const options = { errorRecovery: true, startLine: origin.line, startColumn: origin.column, plugins: plugins(typescript, false) };
  try {
    return parseExpression(code, options) as unknown as Node;
  } catch {
    try {
      return parse(code, { ...options, sourceType: "module", allowReturnOutsideFunction: true }) as unknown as Node;
    } catch {
      return null;
    }
  }
}

/* ------------------------------------------------------------------ walking */

const SKIP_KEYS = new Set([
  "loc",
  "start",
  "end",
  "extra",
  "leadingComments",
  "trailingComments",
  "innerComments",
  "errors",
  "tokens",
  "comments",
]);

/** Visit every node, parents before children, in source order. */
export function walk(root: Node, visit: (node: Node) => void): void {
  const stack: Node[] = [root];
  while (stack.length) {
    const node = stack.pop() as Node;
    visit(node);
    const children: Node[] = [];
    for (const key in node) {
      if (SKIP_KEYS.has(key)) continue;
      const value = node[key];
      if (Array.isArray(value)) {
        for (const item of value) if (item && typeof item.type == "string") children.push(item);
      } else if (value && typeof value.type == "string") children.push(value);
    }
    for (let i = children.length - 1; i >= 0; i--) stack.push(children[i] as Node);
  }
}

/** Strip the wrappers that do not change a value: `as`, `satisfies`, `!`, parentheses. */
export function unwrap(node: Node | null | undefined): Node | null {
  let current = node ?? null;
  while (
    current &&
    (current.type === "TSAsExpression" ||
      current.type === "TSSatisfiesExpression" ||
      current.type === "TSNonNullExpression" ||
      current.type === "TSTypeAssertion" ||
      current.type === "ParenthesizedExpression" ||
      current.type === "AwaitExpression")
  ) {
    current = current.expression ?? current.argument ?? null;
  }
  return current;
}

const isMember = (n: Node | null): n is Node =>
  !!n && (n.type === "MemberExpression" || n.type === "OptionalMemberExpression");
const isCall = (n: Node | null): n is Node =>
  !!n && (n.type === "CallExpression" || n.type === "OptionalCallExpression");

/** The property name of `a.b` or `a["b"]`. */
function memberProp(node: Node): string | null {
  const p = node.property;
  if (!node.computed && p?.type === "Identifier") return p.name;
  if (p?.type === "StringLiteral") return p.value;
  return null;
}

/** The key of an object property, when it is written out. */
function keyName(prop: Node): string | null {
  const key = prop.key;
  if (!key) return null;
  if (!prop.computed && key.type === "Identifier") return key.name;
  if (key.type === "StringLiteral") return key.value;
  if (key.type === "NumericLiteral") return String(key.value);
  return null;
}

/** The value of a string literal or a template without holes. */
function literalString(node: Node | null): string | undefined {
  const n = unwrap(node);
  if (!n) return undefined;
  if (n.type === "StringLiteral") return n.value;
  if (n.type === "TemplateLiteral" && n.expressions.length === 0) {
    return n.quasis.map((q: Node) => q.value.cooked ?? q.value.raw).join("");
  }
  return undefined;
}

/** `window.micaforge`, `globalThis.micaforge`, `self.micaforge`. */
function isGlobalMicaforge(node: Node | null): boolean {
  return (
    isMember(node) &&
    memberProp(node) === "micaforge" &&
    node.object?.type === "Identifier" &&
    GLOBALS.has(node.object.name)
  );
}

/** The name a call reads from: `mf` in `mf.track`, `client` in `this.client.track`. */
function receiverName(node: Node | null): string | null {
  if (!node) return null;
  if (node.type === "Identifier") return node.name;
  if (isMember(node)) return memberProp(node);
  return null;
}

/** The function a call calls, by name: `useMicaforge` in `useMicaforge()`. */
function calleeName(call: Node): string | null {
  const callee = unwrap(call.callee);
  if (callee?.type === "Identifier") return callee.name;
  if (isMember(callee)) return memberProp(callee);
  return null;
}

/** The source text of a node, cut to `max` characters. */
function text(code: string, node: Node, max = 80): string {
  const raw = code.slice(node.start ?? 0, node.end ?? 0).replace(/\s+/g, " ").trim();
  return raw.length > max ? `${raw.slice(0, max - 1)}…` : raw;
}

/* ----------------------------------------------------------------- bindings */

/** Learn the file's names for the SDKs from its imports and declarations. */
export function collectBindings(ctx: FileContext, ast: Node): void {
  walk(ast, (node) => {
    if (node.type === "ImportDeclaration") importBinding(ctx, node.source?.value, node.specifiers ?? []);
  });
  walk(ast, (node) => {
    if (node.type === "VariableDeclarator") declaratorBinding(ctx, node.id, unwrap(node.init));
    else if ((node.type === "ClassProperty" || node.type === "PropertyDefinition") && node.key) {
      const name = keyName(node);
      if (name && isServerConstruction(ctx, unwrap(node.value))) ctx.serverObjs.add(name);
    } else if (node.type === "AssignmentExpression" && isServerConstruction(ctx, unwrap(node.right))) {
      const target = receiverName(unwrap(node.left));
      if (target) ctx.serverObjs.add(target);
    }
  });
}

/** One import, by module and imported name. */
function importBinding(ctx: FileContext, source: unknown, specifiers: Node[]): void {
  if (typeof source != "string") return;
  if (source.startsWith(".")) {
    // The project's own module: a name read from it is resolved after the whole scan.
    for (const spec of specifiers) {
      const local: string | undefined = spec.local?.name;
      if (!local) continue;
      const name =
        spec.type === "ImportNamespaceSpecifier"
          ? "*"
          : spec.type === "ImportDefaultSpecifier"
            ? "default"
            : (spec.imported?.name ?? spec.imported?.value ?? local);
      ctx.imported.set(local, { source, name });
    }
    return;
  }
  const sdk = SDK.test(source);
  const server = SERVER.test(source);
  const nuxt = NUXT.test(source);
  if (!sdk && !server && !nuxt) return;
  if (server) ctx.importsServer = true;

  for (const spec of specifiers) {
    const local: string | undefined = spec.local?.name;
    if (!local) continue;
    const imported: string =
      spec.type === "ImportSpecifier" ? (spec.imported?.name ?? spec.imported?.value ?? local) : spec.type;
    if (sdk) {
      if (spec.type === "ImportNamespaceSpecifier" || spec.type === "ImportDefaultSpecifier") {
        ctx.trackerObjs.set(local, "sdk");
      } else if (imported === "track") ctx.trackFns.set(local, "sdk");
      else if (imported === "useTrackEvent") ctx.trackFactories.add(local);
      else if (imported === "useMicaforge") ctx.trackerFactories.set(local, "composable");
      else if (imported === "getTracker" || imported === "init") ctx.trackerFactories.set(local, "sdk");
      else if (imported === "micaforge") ctx.trackerObjs.set(local, "sdk");
      else if (imported === "TrackClick") ctx.components.add(local);
      else if (imported === "trackClick") ctx.actions.add(local);
    } else if (server) {
      if (spec.type === "ImportNamespaceSpecifier") ctx.serverNamespaces.add(local);
      else if (spec.type === "ImportDefaultSpecifier" || imported === "Micaforge") ctx.serverClasses.add(local);
      else if (imported === "createInstrumentation") ctx.serverFactories.add(local);
    } else if (imported === "useMicaforge") {
      ctx.trackerFactories.set(local, "composable");
    }
  }
}

/** `new Micaforge(...)` or `new ns.Micaforge(...)`. */
function isServerConstruction(ctx: FileContext, node: Node | null): boolean {
  if (node?.type !== "NewExpression") return false;
  const callee = unwrap(node.callee);
  if (callee?.type === "Identifier") return ctx.serverClasses.has(callee.name);
  return (
    isMember(callee) &&
    memberProp(callee) === "Micaforge" &&
    callee.object?.type === "Identifier" &&
    ctx.serverNamespaces.has(callee.object.name)
  );
}

/**
 * The kind of tracker a call returns, if it returns one: `useMicaforge()`, `getTracker()`,
 * `init()`, or the same through a namespace import (`mf.getTracker()`).
 */
function factoryKind(ctx: FileContext, call: Node): SourceKind | undefined {
  const fn = calleeName(call);
  if (!fn) return undefined;
  const callee = unwrap(call.callee);
  if (callee?.type === "Identifier") return ctx.trackerFactories.get(fn);
  const viaSdkNamespace =
    isMember(callee) && callee.object?.type === "Identifier" && ctx.trackerObjs.get(callee.object.name) === "sdk";
  if (!viaSdkNamespace) return undefined;
  if (fn === "getTracker" || fn === "init") return "sdk";
  return fn === "useMicaforge" ? "composable" : undefined;
}

/** Names bound by an object pattern: `{ track, client: mf }` gives `track→track`, `client→mf`. */
function patternNames(pattern: Node): Map<string, string> {
  const out = new Map<string, string>();
  for (const prop of pattern.properties ?? []) {
    if (prop.type !== "ObjectProperty") continue;
    const key = keyName(prop);
    const value = prop.value?.type === "AssignmentPattern" ? prop.value.left : prop.value;
    if (key && value?.type === "Identifier") out.set(key, value.name);
  }
  return out;
}

/** One `const x = …`. */
function declaratorBinding(ctx: FileContext, id: Node | null, init: Node | null): void {
  if (!id || !init) return;
  if (id.type === "Identifier") {
    const value = literalString(init);
    if (value !== undefined) ctx.constStrings.set(id.name, value);
    if (init.type === "ObjectExpression") ctx.constObjects.set(id.name, init);
  }

  // `require("@micaforge/sdk")`, the CommonJS form of an import.
  if (isCall(init) && calleeName(init) === "require") {
    const source = literalString(init.arguments?.[0]);
    if (id.type === "Identifier") {
      importBinding(ctx, source, [{ type: "ImportNamespaceSpecifier", local: { name: id.name } } as Node]);
    } else if (id.type === "ObjectPattern") {
      const specs = [...patternNames(id)].map(
        ([imported, local]) => ({ type: "ImportSpecifier", imported: { name: imported }, local: { name: local } }) as Node,
      );
      importBinding(ctx, source, specs);
    }
    return;
  }

  if (isServerConstruction(ctx, init)) {
    if (id.type === "Identifier") ctx.serverObjs.add(id.name);
    return;
  }
  if (isGlobalMicaforge(init)) {
    if (id.type === "Identifier") ctx.trackerObjs.set(id.name, "global");
    return;
  }
  if (!isCall(init)) return;

  const fn = calleeName(init);
  if (!fn) return;
  if (ctx.trackFactories.has(fn) && id.type === "Identifier") {
    ctx.trackFns.set(id.name, "sdk");
    return;
  }
  const trackerKind = factoryKind(ctx, init);
  if (trackerKind) {
    if (id.type === "Identifier") ctx.trackerObjs.set(id.name, trackerKind);
    else if (id.type === "ObjectPattern") {
      const track = patternNames(id).get("track");
      if (track) ctx.trackFns.set(track, trackerKind);
    }
    return;
  }
  if (ctx.serverFactories.has(fn) && id.type === "ObjectPattern") {
    const client = patternNames(id).get("client");
    if (client) ctx.serverObjs.add(client);
  }
}

/* ------------------------------------------------------------------- names */

/**
 * What the source says a name is: the names, or a computed name with its glob and its
 * text, and the other module's export it reads when it reads one.
 */
export type Resolved = { names: string[] } | { pattern?: string; expression: string; ref?: ModuleRef };

/** Read an event name from an expression. See the module notes for what is read. */
export function resolveNames(ctx: FileContext, raw: Node | null | undefined, code: string): Resolved {
  const node = unwrap(raw);
  if (!node) return { expression: "" };
  const literal = literalString(node);
  if (literal !== undefined) return { names: [literal] };

  if (node.type === "Identifier") {
    const value = ctx.constStrings.get(node.name);
    if (value !== undefined) return { names: [value] };
    const ref = ctx.imported.get(node.name);
    if (ref && ref.name !== "*") return { expression: text(code, node), ref };
  } else if (isMember(node) && node.object?.type === "Identifier") {
    const obj = ctx.constObjects.get(node.object.name);
    const prop = memberProp(node);
    if (obj && prop !== null) {
      const found = (obj.properties ?? []).find((p: Node) => p.type === "ObjectProperty" && keyName(p) === prop);
      const value = found ? literalString(found.value) : undefined;
      if (value !== undefined) return { names: [value] };
    }
    const ref = ctx.imported.get(node.object.name);
    if (ref && prop !== null) {
      return {
        expression: text(code, node),
        ref: ref.name === "*" ? { source: ref.source, name: prop } : { ...ref, member: prop },
      };
    }
  } else if (node.type === "ConditionalExpression") {
    const a = resolveNames(ctx, node.consequent, code);
    const b = resolveNames(ctx, node.alternate, code);
    if ("names" in a && "names" in b) return { names: [...new Set([...a.names, ...b.names])] };
  }

  const parts = templateParts(ctx, node);
  const pattern = parts ? parts.join("").replace(/\*+/g, "*") : undefined;
  return pattern && pattern !== "*" ? { pattern, expression: text(code, node) } : { expression: text(code, node) };
}

/** The fixed and computed parts of a template or a `+` chain, computed parts as `*`. */
function templateParts(ctx: FileContext, node: Node): string[] | null {
  if (node.type === "TemplateLiteral") {
    const out: string[] = [];
    node.quasis.forEach((q: Node, i: number) => {
      out.push(escapeGlob(q.value.cooked ?? q.value.raw));
      if (i < node.expressions.length) {
        const fixed = literalString(node.expressions[i]) ?? constString(ctx, node.expressions[i]);
        out.push(fixed !== undefined ? escapeGlob(fixed) : "*");
      }
    });
    return out;
  }
  if (node.type === "BinaryExpression" && node.operator === "+") {
    const side = (n: Node): string[] => {
      const u = unwrap(n) as Node;
      const fixed = literalString(u) ?? constString(ctx, u);
      if (fixed !== undefined) return [escapeGlob(fixed)];
      return templateParts(ctx, u) ?? ["*"];
    };
    return [...side(node.left), ...side(node.right)];
  }
  return null;
}

function constString(ctx: FileContext, node: Node | null): string | undefined {
  const n = unwrap(node);
  return n?.type === "Identifier" ? ctx.constStrings.get(n.name) : undefined;
}

/** Fixed text for a name pattern. `*` marks a computed part, so a literal one is dropped. */
const escapeGlob = (s: string) => s.replace(/\*/g, "");

/* ------------------------------------------------------------------- props */

/** The properties written in an object expression, and whether some cannot be seen. */
export function propsOf(
  ctx: FileContext,
  raw: Node | null | undefined,
  code: string,
): { props: PropInfo[]; dynamic: boolean } {
  let node = unwrap(raw);
  if (!node) return { props: [], dynamic: false };
  if (node.type === "Identifier") {
    if (node.name === "undefined") return { props: [], dynamic: false };
    const known = ctx.constObjects.get(node.name);
    if (!known) return { props: [], dynamic: true };
    node = known;
  }
  if (node.type !== "ObjectExpression") return { props: [], dynamic: true };

  const props: PropInfo[] = [];
  let dynamic = false;
  for (const prop of node.properties ?? []) {
    if (prop.type === "SpreadElement") {
      dynamic = true;
      continue;
    }
    const key = keyName(prop);
    if (key === null) {
      dynamic = true;
      continue;
    }
    props.push({ key, ...valueInfo(prop.type === "ObjectMethod" ? null : prop.value, code) });
  }
  return { props, dynamic };
}

/** What a property value is, as far as the literal says. */
function valueInfo(raw: Node | null, code: string): Omit<PropInfo, "key"> {
  const node = unwrap(raw);
  if (!node) return { type: "unknown" };
  const literal = literalString(node);
  if (literal !== undefined) return { type: "string", literal };
  switch (node.type) {
    case "TemplateLiteral":
      return { type: "string", expression: text(code, node, 60) };
    case "NumericLiteral":
      return { type: "number", literal: node.value };
    case "BooleanLiteral":
      return { type: "boolean", literal: node.value };
    case "NullLiteral":
      return { type: "null" };
    case "ArrayExpression":
      return { type: "array" };
    case "ObjectExpression":
      return { type: "object" };
    case "UnaryExpression":
      if (node.operator === "-" && node.argument?.type === "NumericLiteral") {
        return { type: "number", literal: -node.argument.value };
      }
      if (node.operator === "!") return { type: "boolean" };
      return { type: "unknown", expression: text(code, node, 60) };
    default:
      return { type: "unknown", expression: text(code, node, 60) };
  }
}

/* ------------------------------------------------------------------- calls */

/** Record one call site for every name the expression can be. */
export function emit(
  ctx: FileContext,
  at: Node | { line: number; column: number },
  source: SourceKind,
  resolved: Resolved,
  props: { props: PropInfo[]; dynamic: boolean },
): void {
  const pos =
    "type" in at
      ? { line: at.loc?.start.line ?? 1, column: (at.loc?.start.column ?? 0) + 1 }
      : (at as { line: number; column: number });
  const base = { file: ctx.file, line: pos.line, column: pos.column, source, props: props.props, propsDynamic: props.dynamic };
  if ("names" in resolved) {
    for (const name of resolved.names) ctx.occurrences.push({ ...base, name });
  } else {
    const occurrence: Occurrence = {
      ...base,
      name: null,
      ...(resolved.pattern ? { pattern: resolved.pattern } : {}),
      expression: resolved.expression,
    };
    ctx.occurrences.push(occurrence);
    if (resolved.ref) ctx.refs.set(occurrence, resolved.ref);
  }
}

/**
 * The string constants a module exports: `export const X = "x"`, the string members of
 * `export const EVENTS = { … }`, and constants exported by name (`export { X }`).
 * Keys are export names; an object's members are `EVENTS.SIGNUP`.
 */
export function exportedStrings(ast: Node): Map<string, string> {
  const locals = new Map<string, string>();
  const objects = new Map<string, Node>();
  const exported = new Map<string, string>();
  const body: Node[] = ast.program?.body ?? [];
  const declare = (decl: Node | null | undefined, exportAll: boolean) => {
    if (decl?.type !== "VariableDeclaration") return;
    for (const d of decl.declarations ?? []) {
      if (d.id?.type !== "Identifier") continue;
      const init = unwrap(d.init);
      const value = literalString(init);
      if (value !== undefined) locals.set(d.id.name, value);
      else if (init?.type === "ObjectExpression") objects.set(d.id.name, init);
      if (exportAll) exported.set(d.id.name, d.id.name);
    }
  };
  for (const statement of body) {
    if (statement.type === "VariableDeclaration") declare(statement, false);
    else if (statement.type === "ExportNamedDeclaration") {
      declare(statement.declaration, true);
      if (!statement.source) {
        for (const spec of statement.specifiers ?? []) {
          const local = spec.local?.name;
          const as = spec.exported?.name ?? spec.exported?.value;
          if (local && as) exported.set(as, local);
        }
      }
    }
  }
  const out = new Map<string, string>();
  for (const [as, local] of exported) {
    const value = locals.get(local);
    if (value !== undefined) out.set(as, value);
    const obj = objects.get(local);
    for (const prop of obj?.properties ?? []) {
      const key = prop.type === "ObjectProperty" ? keyName(prop) : null;
      const member = key !== null ? literalString(prop.value) : undefined;
      if (key !== null && member !== undefined) out.set(`${as}.${key}`, member);
    }
  }
  return out;
}

/** The tracker kind of an identifier, including Vue's `$micaforge` and a Svelte `$store`. */
function trackerKind(ctx: FileContext, name: string): SourceKind | undefined {
  const direct = ctx.trackerObjs.get(name);
  if (direct) return direct;
  if (name.startsWith("$")) {
    const store = ctx.trackerObjs.get(name.slice(1));
    if (store) return store;
    if (name === "$micaforge") return "sdk";
  }
  return undefined;
}

/** `micaforge("event", …)`: the queue snippet's calling convention. */
const isEventWord = (node: Node | undefined) => literalString(node ?? null) === "event";

/** An object expression with a `name` key: the server SDK's `track({ name })`. */
function hasNameKey(node: Node | undefined): boolean {
  const n = unwrap(node);
  return n?.type === "ObjectExpression" && n.properties.some((p: Node) => p.type === "ObjectProperty" && keyName(p) === "name");
}

/** Find every call site in a parsed script or fragment. */
export function findCalls(ctx: FileContext, ast: Node, code: string): void {
  walk(ast, (node) => {
    if (node.type === "CallExpression" || node.type === "OptionalCallExpression") callSite(ctx, node, code);
    else if (node.type === "JSXOpeningElement") jsxSite(ctx, node, code);
    else if ((node.type === "StringLiteral" || node.type === "TemplateLiteral") && stringHasTags(node)) {
      markupString(ctx, node);
    }
  });
}

/** One call. */
function callSite(ctx: FileContext, node: Node, code: string): void {
  const callee = unwrap(node.callee);
  const args: Node[] = node.arguments ?? [];
  const positional = (kind: SourceKind, nameArg: Node | undefined, propsArg: Node | undefined) => {
    if (!nameArg) return;
    emit(ctx, node, kind, resolveNames(ctx, nameArg, code), propsOf(ctx, propsArg, code));
  };

  if (callee?.type === "Identifier") {
    const kind = ctx.trackFns.get(callee.name);
    if (kind) positional(kind, args[0], args[1]);
    else if (callee.name === "micaforge" && isEventWord(args[0])) positional("global", args[1], args[2]);
    return;
  }
  if (!isMember(callee)) return;
  const prop = memberProp(callee);
  const obj = unwrap(callee.object);

  if (prop === "micaforge" && isEventWord(args[0]) && obj?.type === "Identifier" && GLOBALS.has(obj.name)) {
    positional("global", args[1], args[2]);
    return;
  }

  const receiver = receiverName(obj);
  const isServer = receiver !== null && ctx.serverObjs.has(receiver);
  if (prop === "track") {
    if (isServer) return serverTrack(ctx, node, args[0], code);
    if (obj?.type === "Identifier") {
      const kind = trackerKind(ctx, obj.name);
      if (kind) return positional(kind, args[0], args[1]);
    }
    if (isCall(obj)) {
      const kind = factoryKind(ctx, obj);
      if (kind) return positional(kind, args[0], args[1]);
    }
    if (isGlobalMicaforge(obj)) return positional("global", args[0], args[1]);
    // A client imported from your own module: the object form names the server SDK.
    if (hasNameKey(args[0]) && (ctx.importsServer || (receiver !== null && /micaforge/i.test(receiver)))) {
      return serverTrack(ctx, node, args[0], code);
    }
    if (obj?.type === "Identifier" && obj.name === "micaforge") return positional("global", args[0], args[1]);
    return;
  }
  if (!isServer) return;
  if (prop === "pageview" && hasNameKey(args[1])) serverTrack(ctx, node, args[1], code);
  else if (prop === "batch") {
    const list = unwrap(args[0]);
    for (const item of list?.type === "ArrayExpression" ? list.elements : []) {
      const entry = unwrap(item);
      if (entry?.type !== "ObjectExpression") continue;
      const type = entry.properties.find((p: Node) => p.type === "ObjectProperty" && keyName(p) === "type");
      if (literalString(type?.value) === "event" && hasNameKey(entry)) serverTrack(ctx, entry, entry, code);
    }
  }
}

/** The server SDK's object form: `{ name, props, … }`. */
function serverTrack(ctx: FileContext, at: Node, raw: Node | undefined, code: string): void {
  const obj = unwrap(raw);
  if (obj?.type !== "ObjectExpression") return;
  const field = (key: string) =>
    obj.properties.find((p: Node) => p.type === "ObjectProperty" && keyName(p) === key)?.value as Node | undefined;
  const name = field("name");
  if (!name) return;
  emit(ctx, at, "server", resolveNames(ctx, name, code), propsOf(ctx, field("props"), code));
}

/** A JSX element: `data-track` on any element, `event` on `<TrackClick>`. */
function jsxSite(ctx: FileContext, node: Node, code: string): void {
  const attrs: Node[] = (node.attributes ?? []).filter((a: Node) => a.type === "JSXAttribute");
  const attrName = (a: Node): string | null => (a.name?.type === "JSXIdentifier" ? a.name.name : null);
  const valueOf = (a: Node): Node | null => {
    if (!a.value) return null;
    return a.value.type === "JSXExpressionContainer" ? a.value.expression : a.value;
  };

  const tracked = attrs.find((a) => attrName(a) === "data-track");
  if (tracked) {
    const value = valueOf(tracked);
    const resolved: Resolved = value ? resolveNames(ctx, value, code) : { names: [""] };
    const props: PropInfo[] = [];
    for (const a of attrs) {
      const name = attrName(a);
      if (!name?.startsWith("data-track-")) continue;
      const v = unwrap(valueOf(a));
      const literal =
        literalString(v) ??
        (v?.type === "NumericLiteral" || v?.type === "BooleanLiteral" ? String(v.value) : undefined);
      // An attribute is always text in the DOM, whatever JSX expression produced it.
      props.push(
        literal !== undefined
          ? { key: name.slice(11), type: "string", literal }
          : { key: name.slice(11), type: "string", ...(v ? { expression: text(code, v, 60) } : {}) },
      );
    }
    emit(ctx, node, "attribute", resolved, { props, dynamic: false });
  }

  const element = node.name?.type === "JSXIdentifier" ? node.name.name : null;
  if (element && ctx.components.has(element)) {
    const event = attrs.find((a) => attrName(a) === "event");
    if (!event) return;
    const props = attrs.find((a) => attrName(a) === "props");
    emit(ctx, node, "component", resolveNames(ctx, valueOf(event), code), propsOf(ctx, props ? valueOf(props) : null, code));
  }
}

/** HTML in a string or a template literal that carries `data-track`. */
function stringHasTags(node: Node): boolean {
  if (node.type === "StringLiteral") return node.value.includes("data-track");
  return node.quasis.some((q: Node) => (q.value.raw as string).includes("data-track"));
}

/** Placeholder for a template hole while the HTML inside a template literal is read. */
const HOLE = "\u0000";
const HOLES = /\u0000/g;

/** `data-track` attributes in HTML written inside a JavaScript string. */
function markupString(ctx: FileContext, node: Node): void {
  const html: string =
    node.type === "StringLiteral" ? node.value : node.quasis.map((q: Node) => q.value.raw).join(HOLE);
  const lines = new LineIndex(html);
  const startLine = node.loc?.start.line ?? 1;
  // The text starts one character after the opening quote or backtick.
  const firstColumn = (node.loc?.start.column ?? 0) + 1;
  for (const tag of scanMarkup(html, false).tags) {
    const attr = tag.attrs.find((a) => a.name.toLowerCase() === "data-track");
    if (!attr) continue;
    const at = lines.at(tag.start);
    const value = attr.value ?? "";
    const glob = value.replace(HOLES, "*");
    const resolved: Resolved = value.includes(HOLE)
      ? { ...(glob !== "*" ? { pattern: glob } : {}), expression: value.replace(HOLES, "${…}") }
      : { names: [value] };
    const props: PropInfo[] = tag.attrs
      .filter((a) => a.name.toLowerCase().startsWith("data-track-"))
      .map((a): PropInfo =>
        a.value !== null && !a.value.includes(HOLE)
          ? { key: a.name.slice(11), type: "string", literal: a.value }
          : { key: a.name.slice(11), type: "string" },
      );
    const position = {
      line: startLine + at.line - 1,
      column: at.line === 1 ? firstColumn + at.column : at.column,
    };
    emit(ctx, position, "attribute", resolved, { props, dynamic: false });
  }
}
