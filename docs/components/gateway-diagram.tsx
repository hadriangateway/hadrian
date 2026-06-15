"use client";

import {
  createContext,
  useCallback,
  useContext,
  useRef,
  useState,
  useSyncExternalStore,
} from "react";
import Link from "next/link";
import {
  Activity,
  Database,
  FileSearch,
  Fingerprint,
  Gauge,
  Globe,
  Pause,
  Play,
  Plug,
  ScrollText,
  Server,
  ShieldAlert,
  ShieldCheck,
  Split,
  Square,
  Terminal,
  User,
  Wallet,
  Wrench,
} from "lucide-react";
import { Anthropic, AzureAI, Bedrock, Gemini, Ollama, OpenAI, OpenRouter } from "@lobehub/icons";

const basePath = process.env.DOCS_BASE_PATH || "";
const PROVIDERS_DOCS = "/docs/configuration/providers";

// --- Geometry (viewBox units) ---
const VB_W = 960;
const VB_H = 560;
// Headroom added above the scene (negative viewBox min-y) so the top-corner badges
// — the animation toggle and "Slideshow paused" — clear the top provider row, which
// in the busiest scene sits near y=34.
const VB_TOP_PAD = 40;
const UX = 110; // user node center x
const UY = 280; // shared vertical center
const GX = 470; // gateway center x
const GY = 280;
const PX = 770; // provider chip center x
const GW_HALF = 42; // gateway icon half-size
const PROVIDER_HALF = 22; // provider chip radius
const ROW_GAP = 56; // vertical spacing between provider rows

const GW_RIGHT = GX + GW_HALF;
const GW_LEFT = GX - GW_HALF;
const GW_BOTTOM = GY + GW_HALF;
const P_EDGE = PX - PROVIDER_HALF - 2; // x where a provider lane meets its chip

const DOT_FILTER = { filter: "url(#hadrian-dot-glow)" } as const;

// Vertical positions for n provider rows, centered on UY.
const providerYs = (n: number) =>
  Array.from({ length: n }, (_, i) => UY + (i - (n - 1) / 2) * ROW_GAP);

// Gateway out to a provider row.
const providerPath = (y: number) =>
  `M${GW_RIGHT},${GY} C ${GW_RIGHT + 112},${GY} ${GW_RIGHT + 132},${y} ${P_EDGE},${y}`;

// Full lane: user straight through the gateway, then on to one provider. A single
// dot on this path is one request reaching exactly one provider (no split).
const A_IN = { x: UX + 36, y: UY };
const fullPath = (y: number) =>
  `M${A_IN.x},${A_IN.y} L${GW_RIGHT},${GY} C ${GW_RIGHT + 112},${GY} ${GW_RIGHT + 132},${y} ${P_EDGE},${y}`;

// Inbound only: user into the gateway.
const userPath = `M${A_IN.x},${A_IN.y} L${GW_LEFT - 4},${GY}`;

// User to the gateway *centre* — used by rejected requests so the bounce peaks at
// the same point (and the same instant) a passed request changes colour.
const gatePath = `M${A_IN.x},${A_IN.y} L${GX},${GY}`;

// Gateway down to a node directly beneath it (logs, cache, meters).
const SINK_TOP = 384;
const sinkPath = `M${GX},${GW_BOTTOM} L${GX},${SINK_TOP}`;

// =====================================================================
// One shared clock per scene
//
// Every scene is driven by a discrete schedule of request "slots". Slot k
// launches from the user at k·C and crosses the gateway centre at k·C + T_GATE,
// where T_GATE is the (constant) user→gateway travel time. Because that crossing
// time is known analytically, every downstream action — a log row reaching its
// slot, a meter ticking, a request bouncing back — can be pinned to it. Dots,
// logs and meters therefore all live on the single SVG/SMIL timeline and stay in
// lock-step; nothing runs on the independent CSS-animation clock any more.
// =====================================================================

const SPEED = 255; // viewBox units per second — the one speed used everywhere
const IN_FLIGHT = 3; // target number of requests visible at once, per scene

// Length of an "M L … C …" path (straight segments exact, curves sampled).
function pathLength(d: string): number {
  const t = d.match(/[MLC]|-?\d+(?:\.\d+)?/g);
  if (!t) return 0;
  let i = 0;
  const num = () => parseFloat(t[i++]);
  let cx = 0,
    cy = 0,
    len = 0;
  while (i < t.length) {
    const cmd = t[i++];
    if (cmd === "M") {
      cx = num();
      cy = num();
    } else if (cmd === "L") {
      const x = num(),
        y = num();
      // Math.sqrt is IEEE-754 correctly-rounded (deterministic across V8s);
      // Math.hypot is not, which would desync SSR vs client durations.
      len += Math.sqrt((x - cx) * (x - cx) + (y - cy) * (y - cy));
      cx = x;
      cy = y;
    } else if (cmd === "C") {
      const x1 = num(),
        y1 = num(),
        x2 = num(),
        y2 = num(),
        x = num(),
        y = num();
      let px = cx,
        py = cy;
      for (let s = 1; s <= 24; s++) {
        const u = s / 24,
          m = 1 - u;
        const bx = m * m * m * cx + 3 * m * m * u * x1 + 3 * m * u * u * x2 + u * u * u * x;
        const by = m * m * m * cy + 3 * m * m * u * y1 + 3 * m * u * u * y2 + u * u * u * y;
        len += Math.sqrt((bx - px) * (bx - px) + (by - py) * (by - py));
        px = bx;
        py = by;
      }
      cx = x;
      cy = y;
    }
  }
  return len;
}

const travelTime = (d: string) => pathLength(d) / SPEED;
// Fraction of a full lane reached at the gateway centre (where colour changes).
const gateFrac = (d: string) => (GX - A_IN.x) / pathLength(d);

// Constant user→gateway-centre travel time. Same for every lane (the inbound
// segment is identical), so every slot crosses the gateway at k·C + T_GATE.
const T_GATE = (GX - A_IN.x) / SPEED;

// Per-scene cadence and cycle. C keeps ~IN_FLIGHT dots on the longest lane; the
// cycle is n slots long so the whole scene repeats seamlessly every n·C.
function sceneTiming(ys: number[], n: number, inFlight = IN_FLIGHT) {
  const C = Math.max(...ys.map((y) => travelTime(fullPath(y)))) / inFlight;
  return { C, cycle: n * C };
}

// Low-discrepancy lane assignment so a regular launch cadence never reads as a
// top-to-bottom sweep (consecutive slots land on well-separated rows).
const LANE_STEP: Record<number, number> = { 3: 2, 4: 3, 9: 4 };
const laneOf = (k: number, n: number) => (k * (LANE_STEP[n] ?? 1)) % n;

// =====================================================================
// Reduced motion
//
// Dots, glows and bounces are SMIL and carry `motion-reduce:hidden`, so CSS
// hides them with no JS. The logs and meters can't be disabled that way (SMIL
// isn't CSS), so they read this context and render a static frame instead.
// =====================================================================

const ReducedMotionContext = createContext(false);
const useReduced = () => useContext(ReducedMotionContext);

// =====================================================================
// Request-dot primitives (all on the SMIL clock)
// =====================================================================

// A constant-speed request dot: travels `path` once per `dur`, then idles.
function Flow({
  path,
  dur,
  begin,
  className = "fill-fd-primary",
  r = 4.5,
}: {
  path: string;
  dur: number;
  begin: number;
  className?: string;
  r?: number;
}) {
  const travel = Math.min(0.985, travelTime(path) / dur);
  const d = `${dur}s`;
  const b = `${begin}s`;
  return (
    <circle r={r} className={`${className} motion-reduce:hidden`} opacity={0} style={DOT_FILTER}>
      <animateMotion
        path={path}
        dur={d}
        begin={b}
        repeatCount="indefinite"
        calcMode="linear"
        keyPoints="0;1;1"
        keyTimes={`0;${travel};1`}
      />
      <animate
        attributeName="opacity"
        values="0;1;1;0;0"
        keyTimes={`0;0.03;${(travel - 0.03).toFixed(3)};${(travel + 0.01).toFixed(3)};1`}
        dur={d}
        begin={b}
        repeatCount="indefinite"
      />
    </circle>
  );
}

// A constant-speed dot that changes colour as it passes the gateway centre.
function TwoColorFlow({
  path,
  dur,
  begin,
  inClass = "fill-fd-primary",
  outClass,
}: {
  path: string;
  dur: number;
  begin: number;
  inClass?: string;
  outClass: string;
}) {
  const travel = Math.min(0.985, travelTime(path) / dur);
  const tg = Number((gateFrac(path) * travel).toFixed(3));
  const d = `${dur}s`;
  const b = `${begin}s`;
  const motion = (
    <animateMotion
      path={path}
      dur={d}
      begin={b}
      repeatCount="indefinite"
      calcMode="linear"
      keyPoints="0;1;1"
      keyTimes={`0;${travel};1`}
    />
  );
  return (
    <>
      <circle r="4.5" className={`${inClass} motion-reduce:hidden`} opacity={0} style={DOT_FILTER}>
        {motion}
        <animate
          attributeName="opacity"
          values="0;1;1;0;0"
          keyTimes={`0;0.03;${(tg - 0.01).toFixed(3)};${tg};1`}
          dur={d}
          begin={b}
          repeatCount="indefinite"
        />
      </circle>
      <circle r="4.5" className={`${outClass} motion-reduce:hidden`} opacity={0} style={DOT_FILTER}>
        {motion}
        <animate
          attributeName="opacity"
          values="0;0;1;1;0;0"
          keyTimes={`0;${tg};${(tg + 0.01).toFixed(3)};${(travel - 0.01).toFixed(3)};${travel};1`}
          dur={d}
          begin={b}
          repeatCount="indefinite"
        />
      </circle>
    </>
  );
}

// A request that reaches the gateway centre along `path`, then reverses — denied,
// blocked, or rate-limited. Both legs move at SPEED (span derived from length).
function ReturnDot({
  path,
  peak,
  dur,
  begin,
  at,
  inClass = "fill-fd-primary",
  outClass,
}: {
  path: string;
  peak: number;
  dur: number;
  begin: number;
  at: number;
  inClass?: string;
  outClass: string;
}) {
  // Each leg runs at SPEED; clamp only to keep the bounce inside one cycle.
  const span = Math.min((peak * pathLength(path)) / SPEED / dur, at - 0.001, 0.999 - at);
  const t0 = Math.max(0.0001, at - span);
  const keyPoints = `0;0;${peak};0;0`;
  const keyTimes = `0;${t0.toFixed(3)};${at};${(at + span).toFixed(3)};1`;
  const motion = (
    <animateMotion
      path={path}
      dur={`${dur}s`}
      begin={`${begin}s`}
      repeatCount="indefinite"
      calcMode="linear"
      keyPoints={keyPoints}
      keyTimes={keyTimes}
    />
  );
  return (
    <>
      <circle r="4.5" className={`${inClass} motion-reduce:hidden`} opacity={0} style={DOT_FILTER}>
        {motion}
        <animate
          attributeName="opacity"
          values="0;0;1;1;0;0"
          keyTimes={`0;${t0.toFixed(3)};${(t0 + 0.02).toFixed(3)};${(at - 0.01).toFixed(3)};${at};1`}
          dur={`${dur}s`}
          begin={`${begin}s`}
          repeatCount="indefinite"
        />
      </circle>
      <circle r="4.5" className={`${outClass} motion-reduce:hidden`} opacity={0} style={DOT_FILTER}>
        {motion}
        <animate
          attributeName="opacity"
          values="0;0;1;1;0;0"
          keyTimes={`0;${at};${(at + 0.02).toFixed(3)};${(at + span - 0.01).toFixed(3)};${(at + span).toFixed(3)};1`}
          dur={`${dur}s`}
          begin={`${begin}s`}
          repeatCount="indefinite"
        />
      </circle>
    </>
  );
}

// A halo that flashes as a dot reaches a node (`at` = arrival fraction of dur).
function NodeGlow({
  x,
  y,
  size,
  dur,
  begin,
  at,
}: {
  x: number;
  y: number;
  size: number;
  dur: number;
  begin: number;
  at: number;
}) {
  // A fixed ~0.55s pulse that peaks exactly as the dot arrives (`at`), so the
  // glow stays in step whatever the lane period is.
  const a1 = Math.max(0, at - 0.16 / dur).toFixed(4);
  const a2 = at.toFixed(4);
  const a3 = Math.min(1, at + 0.4 / dur).toFixed(4);
  return (
    <rect
      x={x - size / 2}
      y={y - size / 2}
      width={size}
      height={size}
      rx={size / 3}
      aria-hidden="true"
      className="fill-fd-primary motion-reduce:hidden"
      opacity={0}
      style={{ filter: "url(#hadrian-node-glow)" }}
    >
      <animate
        attributeName="opacity"
        values="0;0;0.7;0;0"
        keyTimes={`0;${a1};${a2};${a3};1`}
        dur={`${dur}s`}
        begin={`${begin}s`}
        repeatCount="indefinite"
      />
    </rect>
  );
}

// Slot k as a forward request: launches at k·C, optionally turns `outClass` at
// the gateway, and lights its destination as it arrives.
function ForwardDot({
  y,
  begin,
  cycle,
  className = "fill-fd-primary",
  outClass,
  r,
}: {
  y: number;
  begin: number;
  cycle: number;
  className?: string;
  outClass?: string;
  r?: number;
}) {
  const path = fullPath(y);
  const at = Math.min(0.985, travelTime(path) / cycle);
  return (
    <g>
      <NodeGlow x={PX} y={y} size={56} dur={cycle} begin={begin} at={at} />
      {outClass ? (
        <TwoColorFlow
          path={path}
          dur={cycle}
          begin={begin}
          inClass={className}
          outClass={outClass}
        />
      ) : (
        <Flow path={path} dur={cycle} begin={begin} className={className} r={r} />
      )}
    </g>
  );
}

// Slot k as a rejected request: reaches the gateway centre at k·C + T_GATE, then
// bounces back in `outClass`.
function BounceDot({ begin, cycle, outClass }: { begin: number; cycle: number; outClass: string }) {
  return (
    <ReturnDot
      path={gatePath}
      peak={1}
      dur={cycle}
      begin={begin}
      at={Math.min(0.985, T_GATE / cycle)}
      outClass={outClass}
    />
  );
}

// =====================================================================
// Node chips
// =====================================================================

function UserNode({ label = "Your users" }: { label?: string }) {
  return (
    <>
      <foreignObject x={UX - 34} y={UY - 34} width={68} height={68} aria-hidden="true">
        <div className="flex h-full w-full items-center justify-center rounded-2xl border border-fd-border bg-fd-card shadow-sm">
          <User className="h-7 w-7 text-fd-muted-foreground" strokeWidth={1.5} />
        </div>
      </foreignObject>
      <text
        x={UX}
        y={UY + 54}
        textAnchor="middle"
        className="fill-fd-foreground"
        fontSize={16}
        fontWeight={600}
      >
        {label}
      </text>
    </>
  );
}

function GatewayNode() {
  return (
    <>
      <image
        href={`${basePath}/icon.svg`}
        x={GX - GW_HALF}
        y={GY - GW_HALF}
        width={GW_HALF * 2}
        height={GW_HALF * 2}
      />
      <text
        x={GX}
        y={GW_BOTTOM + 30}
        textAnchor="middle"
        className="fill-fd-foreground"
        fontSize={18}
        fontWeight={700}
      >
        Hadrian Gateway
      </text>
    </>
  );
}

type Provider = {
  name: string;
  node: React.ReactNode;
  href: string;
};

// =====================================================================
// Region flags — emoji, bumped up from the 11px tag text for legibility.
// =====================================================================

function FlagEmoji({ children }: { children: string }) {
  return (
    <span className="text-[15px] leading-none" aria-hidden="true">
      {children}
    </span>
  );
}

const UsFlag = () => <FlagEmoji>🇺🇸</FlagEmoji>;
const EuFlag = () => <FlagEmoji>🇪🇺</FlagEmoji>;
const AuFlag = () => <FlagEmoji>🇦🇺</FlagEmoji>;

function Chip({
  provider,
  y,
  tag,
  tagColor,
  flag,
  region,
  unhealthy,
}: {
  provider: Provider;
  y: number;
  tag?: string;
  tagColor?: string;
  flag?: React.ReactNode;
  region?: string;
  unhealthy?: boolean;
}) {
  return (
    <foreignObject
      x={PX - PROVIDER_HALF}
      y={y - PROVIDER_HALF}
      width={VB_W - (PX - PROVIDER_HALF)}
      height={PROVIDER_HALF * 2}
    >
      <Link
        href={provider.href}
        aria-label={`${provider.name} provider documentation`}
        className="group flex h-full items-center gap-3 no-underline"
      >
        <span
          className={`flex aspect-square h-full flex-none items-center justify-center rounded-xl border bg-fd-card shadow-sm transition-colors ${
            unhealthy
              ? "border-red-500/50 opacity-60"
              : "border-fd-border group-hover:border-fd-primary/60"
          }`}
        >
          {provider.node}
        </span>
        <span className="flex flex-col leading-tight">
          <span
            className={`font-medium transition-colors group-hover:text-fd-foreground ${
              unhealthy ? "text-fd-muted-foreground/60" : "text-fd-muted-foreground"
            }`}
            style={{ fontSize: 14 }}
          >
            {provider.name}
          </span>
          {(tag || region) && (
            <span className="flex items-center gap-1.5 text-[11px]">
              {tag && (
                <span className="flex items-center gap-1 uppercase tracking-wide text-fd-muted-foreground/70">
                  {flag ?? (
                    <span
                      className="inline-block h-2 w-2 rounded-full"
                      style={{ background: tagColor }}
                    />
                  )}
                  {tag}
                </span>
              )}
              {region && <span className="text-fd-muted-foreground/50">{region}</span>}
            </span>
          )}
        </span>
      </Link>
    </foreignObject>
  );
}

function ProviderChips({ providers, ys }: { providers: Provider[]; ys?: number[] }) {
  const rowYs = ys ?? providerYs(providers.length);
  return (
    <>
      {providers.map((p, i) => (
        <Chip key={p.name} provider={p} y={rowYs[i]} />
      ))}
    </>
  );
}

function Wires({ ys }: { ys: number[] }) {
  return (
    <g fill="none" aria-hidden="true" className="stroke-fd-border" strokeWidth={1.5}>
      <path d={userPath} />
      {ys.map((y, i) => (
        <path key={i} d={providerPath(y)} />
      ))}
    </g>
  );
}

function SinkWire() {
  return (
    <g fill="none" aria-hidden="true" className="stroke-fd-border" strokeWidth={1.5}>
      <path d={sinkPath} strokeDasharray="4 4" />
    </g>
  );
}

function SatelliteNode({
  x,
  y,
  w,
  h,
  icon,
  title,
}: {
  x: number;
  y: number;
  w: number;
  h: number;
  icon: React.ReactNode;
  title: string;
}) {
  return (
    <foreignObject x={x - w / 2} y={y - h / 2} width={w} height={h} aria-hidden="true">
      <div className="flex h-full w-full items-center gap-2.5 rounded-xl border border-fd-border bg-fd-card px-3 shadow-sm">
        <span className="flex-none text-fd-muted-foreground">{icon}</span>
        <span className="text-[13px] font-semibold text-fd-foreground">{title}</span>
      </div>
    </foreignObject>
  );
}

// =====================================================================
// Action nodes — driven by the slot schedule, on the SMIL clock
// =====================================================================

const LOG_ROW_H = 16;
const LOG_VISIBLE = 3;

// A live log pinned under the gateway. Each entry *pops in* at the top the
// instant its request crosses the gateway (row k begins at k·C + T_GATE — the
// same crossing time its dot rides), the older entries step down one slot, and
// the oldest leaves off the bottom. The list therefore advances as a discrete
// event per request, not a continuous marquee, and the row that appears *is* the
// request you just watched pass through.
function EventLog({
  id,
  w,
  title,
  icon,
  href,
  rows,
  C,
  cycle,
}: {
  id: string;
  w: number;
  title: string;
  icon: React.ReactNode;
  href?: string;
  rows: React.ReactNode[];
  C: number;
  cycle: number;
}) {
  const reduced = useReduced();
  const n = rows.length;
  const headerH = 26;
  const cardX = GX - w / 2;
  const cardY = SINK_TOP;
  const rowsX = cardX + 12;
  const rowsW = w - 24;
  const rowsTopY = cardY + headerH;
  const cardH = headerH + LOG_VISIBLE * LOG_ROW_H + 12;
  const clipId = `hadrian-logclip-${id}`;

  const header = (
    <span className="flex items-center gap-1.5 text-[12px] font-semibold text-fd-foreground">
      <span className="text-fd-muted-foreground">{icon}</span>
      {title}
    </span>
  );

  const rowDiv = (key: string, content: React.ReactNode) => (
    <div
      key={key}
      className="flex items-center gap-1.5 overflow-hidden whitespace-nowrap"
      style={{ height: LOG_ROW_H }}
    >
      {content}
    </div>
  );

  return (
    <>
      <rect
        x={cardX}
        y={cardY}
        width={w}
        height={cardH}
        rx={12}
        className="fill-fd-card stroke-fd-border"
        strokeWidth={1.5}
      />
      <foreignObject
        x={cardX}
        y={cardY + 6}
        width={w}
        height={headerH}
        aria-hidden={href ? undefined : "true"}
      >
        <div className="px-3">
          {href ? (
            <Link
              href={href}
              className="inline-flex no-underline transition-colors hover:text-fd-primary"
            >
              {header}
            </Link>
          ) : (
            header
          )}
        </div>
      </foreignObject>

      {reduced ? (
        <foreignObject
          x={rowsX}
          y={rowsTopY}
          width={rowsW}
          height={LOG_VISIBLE * LOG_ROW_H}
          aria-hidden="true"
        >
          <div className="font-mono text-[10.5px]">
            {rows.slice(0, LOG_VISIBLE).map((row, i) => rowDiv(`s-${i}`, row))}
          </div>
        </foreignObject>
      ) : (
        <>
          <clipPath id={clipId}>
            <rect x={rowsX} y={rowsTopY} width={rowsW} height={LOG_VISIBLE * LOG_ROW_H} />
          </clipPath>
          <g clipPath={`url(#${clipId})`} aria-hidden="true">
            {rows.map((row, e) => {
              // Slot 0 is the top row, slot LOG_VISIBLE-1 the freshest at the bottom.
              const slotY = (slot: number) => rowsTopY + slot * LOG_ROW_H;
              const cad = 1 / n; // one cadence as a fraction of the cycle
              const pop = 0.16 / cycle; // pop-in / step / fade-out durations
              const life = LOG_VISIBLE * cad; // visible for LOG_VISIBLE cadences
              const bottom = LOG_VISIBLE - 1;
              const RISE = 7; // px the row rises while popping in from below
              const clamp = (t: number) => Math.min(1, Math.max(0, t));

              // Pop in at the top, then step down one slot per cadence, then exit
              // off the bottom — each move a quick ease the moment the next row lands.
              const move: [number, number][] = [
                [0, slotY(0) - RISE],
                [pop, slotY(0)],
              ];
              for (let s = 1; s < LOG_VISIBLE; s++) {
                move.push([s * cad, slotY(s - 1)]);
                move.push([s * cad + pop, slotY(s)]);
              }
              move.push([life, slotY(bottom)]);
              move.push([clamp(life + pop), slotY(bottom) + LOG_ROW_H]);
              move.push([1, slotY(bottom) + LOG_ROW_H]);

              const fade: [number, number][] = [
                [0, 0],
                [pop, 1],
                [life, 1],
                [clamp(life + pop), 0],
                [1, 0],
              ];

              const begin = (e - n) * C + T_GATE; // already running, periodic at t≥0
              const kt = (kf: [number, number][]) => kf.map(([t]) => clamp(t).toFixed(4)).join(";");
              return (
                <g key={e} opacity={0}>
                  <animateTransform
                    attributeName="transform"
                    type="translate"
                    calcMode="linear"
                    values={move.map(([, y]) => `0 ${y.toFixed(2)}`).join(";")}
                    keyTimes={kt(move)}
                    dur={`${cycle}s`}
                    begin={`${begin}s`}
                    repeatCount="indefinite"
                  />
                  <animate
                    attributeName="opacity"
                    calcMode="linear"
                    values={fade.map(([, o]) => o).join(";")}
                    keyTimes={kt(fade)}
                    dur={`${cycle}s`}
                    begin={`${begin}s`}
                    repeatCount="indefinite"
                  />
                  <foreignObject x={rowsX} y={0} width={rowsW} height={LOG_ROW_H}>
                    <div className="font-mono text-[10.5px]">{rowDiv(`r-${e}`, row)}</div>
                  </foreignObject>
                </g>
              );
            })}
          </g>
        </>
      )}
    </>
  );
}

// A meter under the gateway whose fill is the request schedule made cumulative:
// each accepted slot nudges the bar at its crossing time, the gaps between slots
// are the (linear) refill/leak, and `keyTimes`/`values` come straight from the
// per-scene simulation that also decides which slots bounce.
function EventMeter({
  w,
  icon,
  title,
  keyTimes,
  values,
  cycle,
  balanceSteps,
  staticFrac,
  staticBalance,
}: {
  w: number;
  icon: React.ReactNode;
  title: string;
  keyTimes: number[];
  values: number[];
  cycle: number;
  balanceSteps?: { from: number; to: number; text: string }[];
  staticFrac: number;
  staticBalance?: string;
}) {
  const reduced = useReduced();
  const h = 56;
  const cardX = GX - w / 2;
  const cardY = SINK_TOP;
  const fillW = w - 24;
  const barX = cardX + 12;
  const barY = cardY + 38;
  const barH = 6;

  const widths = values.map((v) => (fillW * Math.max(0, Math.min(1, v))).toFixed(1));
  const kt = keyTimes.map((t) => Math.max(0, Math.min(1, t)).toFixed(4)).join(";");

  return (
    <>
      <rect
        x={cardX}
        y={cardY}
        width={w}
        height={h}
        rx={12}
        className="fill-fd-card stroke-fd-border"
        strokeWidth={1.5}
      />
      <foreignObject x={cardX} y={cardY + 8} width={w} height={20} aria-hidden="true">
        <div className="flex items-center gap-2 px-3 text-[13px] font-semibold text-fd-foreground">
          <span className="text-fd-muted-foreground">{icon}</span>
          {title}
        </div>
      </foreignObject>

      {/* Balance counter (right) — one text node per step, opacity-gated on the
          shared clock so it never drifts from the bar. */}
      {reduced
        ? staticBalance && (
            <text
              x={cardX + w - 12}
              y={cardY + 22}
              textAnchor="end"
              className="fill-fd-muted-foreground font-mono"
              fontSize={11}
              aria-hidden="true"
            >
              {staticBalance}
            </text>
          )
        : balanceSteps?.map((b, i) => {
            const from = Math.max(0.0001, Math.min(0.999, b.from));
            const to = Math.max(from + 0.0002, Math.min(1, b.to));
            return (
              <text
                key={i}
                x={cardX + w - 12}
                y={cardY + 22}
                textAnchor="end"
                className="fill-fd-muted-foreground font-mono"
                fontSize={11}
                opacity={0}
                aria-hidden="true"
              >
                {b.text}
                <animate
                  attributeName="opacity"
                  values="0;0;1;1;0;0"
                  keyTimes={`0;${from.toFixed(4)};${from.toFixed(4)};${to.toFixed(4)};${to.toFixed(4)};1`}
                  dur={`${cycle}s`}
                  repeatCount="indefinite"
                />
              </text>
            );
          })}

      <rect x={barX} y={barY} width={fillW} height={barH} rx={barH / 2} className="fill-fd-muted" />
      <rect
        x={barX}
        y={barY}
        width={reduced ? fillW * staticFrac : widths[0]}
        height={barH}
        rx={barH / 2}
        className="fill-fd-primary"
      >
        {!reduced && (
          <animate
            attributeName="width"
            values={widths.join(";")}
            keyTimes={kt}
            calcMode="linear"
            dur={`${cycle}s`}
            repeatCount="indefinite"
          />
        )}
      </rect>
    </>
  );
}

// =====================================================================
// Log row renderers (HTML, inside the scrolling strip)
// =====================================================================

function Tag({ tone, children }: { tone: "allow" | "deny" | "redact"; children: React.ReactNode }) {
  const map = {
    allow: ["#16a34a", "rgba(22,163,74,0.12)"],
    deny: ["#dc2626", "rgba(220,38,38,0.12)"],
    redact: ["#d97706", "rgba(217,119,6,0.12)"],
  } as const;
  const [color, background] = map[tone];
  return (
    <span className="rounded px-1 font-semibold" style={{ color, background }}>
      {children}
    </span>
  );
}

function PolicyRow({
  tone,
  action,
  policy,
}: {
  tone: "allow" | "deny";
  action: string;
  policy: string;
}) {
  return (
    <>
      <Tag tone={tone}>{tone}</Tag>
      <span className="w-[116px] text-fd-foreground">{action}</span>
      <span className="ml-auto text-fd-muted-foreground">{policy}</span>
    </>
  );
}

function ScreenRow({ tone, label }: { tone: "allow" | "deny" | "redact"; label: string }) {
  return (
    <>
      <Tag tone={tone}>{tone}</Tag>
      <span className={tone === "allow" ? "text-fd-muted-foreground" : "text-fd-foreground"}>
        {label}
      </span>
    </>
  );
}

function UsageRow({
  identity,
  provider,
  tok,
  finish,
  cost,
  lat,
}: {
  identity: string;
  provider: string;
  tok: string;
  finish: string;
  cost: string;
  lat: string;
}) {
  return (
    <>
      <span className="w-[112px] text-fd-foreground">{identity}</span>
      <span className="w-[62px] text-fd-muted-foreground">{provider}</span>
      <span className="w-[70px] text-fd-muted-foreground">{tok}</span>
      <span className="w-[94px] text-fd-muted-foreground">{finish}</span>
      <span className="w-[50px] text-fd-foreground">{cost}</span>
      <span className="ml-auto text-fd-muted-foreground">{lat}</span>
    </>
  );
}

// =====================================================================
// Provider catalogue
// =====================================================================

const ALL: Record<string, Provider> = {
  bedrock: {
    name: "Amazon Bedrock",
    node: <Bedrock.Color size={22} />,
    href: `${PROVIDERS_DOCS}#aws-bedrock`,
  },
  anthropic: {
    name: "Anthropic",
    node: <Anthropic size={20} style={{ color: "#D97757" }} />,
    href: `${PROVIDERS_DOCS}#anthropic`,
  },
  azure: {
    name: "Azure OpenAI",
    node: <AzureAI.Color size={22} />,
    href: `${PROVIDERS_DOCS}#azure-openai`,
  },
  gemini: {
    name: "Google Gemini",
    node: <Gemini.Color size={22} />,
    href: `${PROVIDERS_DOCS}#google-vertex-ai`,
  },
  openai: {
    name: "OpenAI",
    node: <OpenAI size={20} className="text-fd-foreground" />,
    href: `${PROVIDERS_DOCS}#openai`,
  },
  openrouter: {
    name: "OpenRouter",
    node: <OpenRouter size={20} style={{ color: "#6566F1" }} />,
    href: `${PROVIDERS_DOCS}#openai-compatible-providers`,
  },
  ollama: {
    name: "Ollama",
    node: <Ollama size={20} className="text-fd-foreground" />,
    href: `${PROVIDERS_DOCS}#openai-compatible-providers`,
  },
  onprem: {
    name: "On-prem",
    node: <Server size={18} strokeWidth={1.75} className="text-fd-muted-foreground" />,
    href: `${PROVIDERS_DOCS}#openai-compatible-providers`,
  },
  compatible: {
    name: "OpenAI-compatible",
    node: <Plug size={18} strokeWidth={1.75} className="text-fd-muted-foreground" />,
    href: `${PROVIDERS_DOCS}#openai-compatible-providers`,
  },
};

const ROUTING_SET = [
  ALL.bedrock,
  ALL.anthropic,
  ALL.azure,
  ALL.gemini,
  ALL.ollama,
  ALL.onprem,
  ALL.openai,
  ALL.compatible,
  ALL.openrouter,
];
// Lane order: anthropic, openai, gemini. Usage/scene rows reference these by name.
const LEAN_SET = [ALL.anthropic, ALL.openai, ALL.gemini];

// =====================================================================
// Meter schedules (deterministic, no Date/Math.random)
//
// Meter scenes run on a slightly longer cycle (`meterCycle`) than n·C: the extra
// T_GATE tail is an arrival-free gap that (a) keeps every slot's crossing
// fraction inside [0,1) — so no keyTime needs clamping — and (b) gives the meter
// a moment to recover/leak so the loop is seamless.
// =====================================================================

const meterCycle = (C: number, n: number) => n * C + T_GATE;

// Build a stepped bar trajectory: the bar holds, then jumps at each event's
// crossing fraction, so it visibly moves the instant a request passes the
// gateway. `holdToEnd` keeps the last value pinned until the cycle boundary
// (a hard reset, e.g. a new budget period) instead of leaking back down.
function stepMeter(
  events: { f: number; level: number }[],
  start: number,
  end: number,
  holdToEnd = false
) {
  const keyTimes = [0];
  const values = [start];
  let prev = start;
  for (const e of events) {
    keyTimes.push(Math.max(0.0001, e.f - 0.0015));
    values.push(prev);
    keyTimes.push(e.f);
    values.push(e.level);
    prev = e.level;
  }
  if (holdToEnd) {
    keyTimes.push(0.999);
    values.push(prev);
  }
  keyTimes.push(1);
  values.push(end);
  return { keyTimes, values };
}

// Rate limiting: load climbs as requests arrive, tops out, and the requests
// that arrive while full are shed (bounced). It leaks back down over the tail.
function rateSchedule(C: number, cycle: number) {
  const acceptLevels = [0.28, 0.45, 0.6, 0.74, 0.87, 1.0];
  const shed = [false, false, false, false, false, false, true, true];
  let prev = 0.1;
  let li = 0;
  const events = shed.map((isShed, k) => {
    const level = isShed ? prev : acceptLevels[li++];
    prev = level;
    return { f: (k * C + T_GATE) / cycle, level };
  });
  return { ...stepMeter(events, 0.1, 0.1), shed };
}

// Cumulative spend against a fixed budget; a slot whose cost would overflow the
// budget is shed. The bar holds the running total, then resets at the boundary.
function budgetSchedule(costs: number[], budget: number, C: number, cycle: number) {
  let spent = 0;
  let segFrom = 0;
  const shed: boolean[] = [];
  const events: { f: number; level: number }[] = [];
  const balanceSteps: { from: number; to: number; text: string }[] = [];
  const fmt = (v: number) => `$${v.toFixed(2)} / $${budget.toLocaleString("en-US")}`;
  for (let k = 0; k < costs.length; k++) {
    const f = (k * C + T_GATE) / cycle;
    balanceSteps.push({ from: segFrom, to: f, text: fmt(spent) });
    const accept = spent + costs[k] <= budget;
    if (accept) spent += costs[k];
    shed.push(!accept);
    events.push({ f, level: spent / budget });
    segFrom = f;
  }
  balanceSteps.push({ from: segFrom, to: 1, text: fmt(spent) });
  return { ...stepMeter(events, 0, 0, true), shed, balanceSteps };
}

// =====================================================================
// Scenes
// =====================================================================

type Scene = {
  id: string;
  pill: string;
  caption: string;
  href: string;
  render: () => React.ReactNode;
};

const scenes: Scene[] = [
  {
    id: "routing",
    pill: "Routing",
    caption: "One OpenAI-compatible API routes to any provider.",
    href: PROVIDERS_DOCS,
    render: () => {
      const ys = providerYs(ROUTING_SET.length);
      const n = ROUTING_SET.length;
      const { C, cycle } = sceneTiming(ys, n);
      return (
        <>
          <Wires ys={ys} />
          {ROUTING_SET.map((_, k) => {
            const lane = laneOf(k, n);
            return <ForwardDot key={k} y={ys[lane]} begin={k * C} cycle={cycle} />;
          })}
          <UserNode />
          <GatewayNode />
          <ProviderChips providers={ROUTING_SET} ys={ys} />
        </>
      );
    },
  },
  {
    id: "failover",
    pill: "Failover",
    caption:
      "Health checks flag unhealthy providers, so the gateway routes around them to healthy ones.",
    href: `${PROVIDERS_DOCS}#health-checks`,
    render: () => {
      // Several regional copies of each provider. Health checks dark out the
      // unhealthy regions, so the gateway routes around them to a healthy copy.
      const providers = [
        { p: ALL.bedrock, region: "us-east-1", healthy: true },
        { p: ALL.bedrock, region: "eu-central-1", healthy: true },
        { p: ALL.openai, region: "US", healthy: false },
        { p: ALL.openai, region: "EU", healthy: true },
        { p: ALL.azure, region: "swedencentral", healthy: true },
        { p: ALL.azure, region: "francecentral", healthy: false },
      ];
      const ys = providerYs(providers.length);
      const healthyRows = providers.map((h, i) => (h.healthy ? i : -1)).filter((i) => i >= 0);
      const n = 8;
      const { C, cycle } = sceneTiming(
        healthyRows.map((i) => ys[i]),
        n
      );
      return (
        <>
          <g fill="none" aria-hidden="true" strokeWidth={1.5}>
            <path d={userPath} className="stroke-fd-border" />
            {providers.map((h, i) => (
              <path
                key={i}
                d={providerPath(ys[i])}
                className={h.healthy ? "stroke-fd-border" : "stroke-red-500/40"}
                strokeDasharray={h.healthy ? undefined : "4 4"}
              />
            ))}
          </g>
          {Array.from({ length: n }, (_, k) => (
            <ForwardDot
              key={k}
              y={ys[healthyRows[laneOf(k, healthyRows.length)]]}
              begin={k * C}
              cycle={cycle}
            />
          ))}
          <UserNode />
          <GatewayNode />
          {providers.map((h, i) => (
            <Chip
              key={`${h.p.name}-${h.region}`}
              provider={h.p}
              y={ys[i]}
              tag={h.healthy ? "Healthy" : "Unhealthy"}
              tagColor={h.healthy ? "#22c55e" : "#ef4444"}
              region={h.region}
              unhealthy={!h.healthy}
            />
          ))}
        </>
      );
    },
  },
  {
    id: "sovereignty",
    pill: "Sovereignty",
    caption:
      "Requests route only to providers in compliant regions, based on the sovereignty rules you define.",
    href: "/docs/features/data-sovereignty",
    render: () => {
      const rows = [
        {
          p: ALL.bedrock,
          region: "AU",
          flag: <AuFlag />,
          fill: "fill-[#FFCD00]",
          stroke: "stroke-[#FFCD00]/70",
        },
        {
          p: ALL.onprem,
          region: "AU",
          flag: <AuFlag />,
          fill: "fill-[#FFCD00]",
          stroke: "stroke-[#FFCD00]/70",
        },
        {
          p: ALL.azure,
          region: "EU",
          flag: <EuFlag />,
          fill: "fill-blue-500",
          stroke: "stroke-blue-500/70",
        },
        {
          p: ALL.gemini,
          region: "EU",
          flag: <EuFlag />,
          fill: "fill-blue-500",
          stroke: "stroke-blue-500/70",
        },
        {
          p: ALL.openai,
          region: "US",
          flag: <UsFlag />,
          fill: "fill-red-500",
          stroke: "stroke-red-500/70",
        },
        {
          p: ALL.anthropic,
          region: "US",
          flag: <UsFlag />,
          fill: "fill-red-500",
          stroke: "stroke-red-500/70",
        },
      ];
      const ys = providerYs(rows.length);
      const n = 8;
      const { C, cycle } = sceneTiming(ys, n);
      return (
        <>
          <g fill="none" aria-hidden="true" strokeWidth={1.5}>
            <path d={userPath} className="stroke-fd-border" />
            {rows.map((r, i) => (
              <path key={i} d={providerPath(ys[i])} className={r.stroke} />
            ))}
          </g>
          {/* Each request is one colour and travels to a single matching provider. */}
          {Array.from({ length: n }, (_, k) => {
            const lane = laneOf(k, rows.length);
            return (
              <ForwardDot
                key={k}
                y={ys[lane]}
                begin={k * C}
                cycle={cycle}
                className={rows[lane].fill}
              />
            );
          })}
          <UserNode />
          <GatewayNode />
          {rows.map((r, i) => (
            <Chip
              key={`${r.p.name}-${r.region}`}
              provider={r.p}
              y={ys[i]}
              tag={r.region}
              flag={r.flag}
            />
          ))}
        </>
      );
    },
  },
  {
    id: "auth",
    pill: "Authentication",
    caption:
      "Every request is authenticated by API key or single sign-on with your identity provider before it reaches a model.",
    href: "/docs/authentication",
    render: () => {
      const ys = providerYs(LEAN_SET.length);
      const n = 6;
      const { C, cycle } = sceneTiming(ys, n);
      const idpY = 116;
      const loginPath = `M${UX},${UY - 34} L${UX},${idpY + 22}`;
      const idpToGw = `M${UX + 80},${idpY} C ${UX + 190},${idpY} ${GW_LEFT - 70},${GY - 26} ${GW_LEFT - 4},${GY - 18}`;
      return (
        <>
          <Wires ys={ys} />
          {/* IdP wired to the user (neutral) and the gateway (green: trusted identity). */}
          <g fill="none" aria-hidden="true" strokeWidth={1.5}>
            <path d={loginPath} strokeDasharray="4 4" className="stroke-fd-border" />
            <path d={idpToGw} strokeDasharray="4 4" className="stroke-emerald-500/70" />
          </g>
          {/* Authenticated requests are green from the user onward. */}
          {Array.from({ length: n }, (_, k) => (
            <ForwardDot
              key={k}
              y={ys[laneOf(k, LEAN_SET.length)]}
              begin={k * C}
              cycle={cycle}
              className="fill-emerald-500"
            />
          ))}
          {/* Occasional, irregular login traffic to the IdP. */}
          <Flow path={loginPath} dur={5.3} begin={0.6} />
          <Flow path={loginPath} dur={7.1} begin={3.4} />
          <UserNode />
          <GatewayNode />
          <ProviderChips providers={LEAN_SET} ys={ys} />
          <SatelliteNode
            x={UX}
            y={idpY}
            w={150}
            h={44}
            icon={<Fingerprint className="h-5 w-5" />}
            title="Identity Provider"
          />
        </>
      );
    },
  },
  {
    id: "authz",
    pill: "Authorization",
    caption: "CEL-based RBAC evaluates system and org policies to allow or deny each request.",
    href: "/docs/features/authorization",
    render: () => {
      const ys = providerYs(LEAN_SET.length);
      const items: {
        tone: "allow" | "deny";
        action: string;
        policy: string;
        lane?: number;
      }[] = [
        { tone: "allow", action: "model:use", policy: "org-member-read", lane: 0 },
        { tone: "deny", action: "model:use", policy: "premium-models" },
        { tone: "allow", action: "vector_store:read", policy: "org-member-read", lane: 2 },
        { tone: "allow", action: "response:delete", policy: "org-admin", lane: 1 },
        { tone: "deny", action: "user:delete", policy: "deny-self-delete" },
        { tone: "allow", action: "model:use", policy: "org-member-read", lane: 0 },
      ];
      const n = items.length;
      const { C, cycle } = sceneTiming(ys, n);
      return (
        <>
          <Wires ys={ys} />
          {items.map((it, k) =>
            it.tone === "allow" ? (
              <ForwardDot
                key={k}
                y={ys[it.lane ?? 0]}
                begin={k * C}
                cycle={cycle}
                outClass="fill-emerald-500"
              />
            ) : (
              <BounceDot key={k} begin={k * C} cycle={cycle} outClass="fill-red-500" />
            )
          )}
          <UserNode />
          <GatewayNode />
          <ProviderChips providers={LEAN_SET} ys={ys} />
          <SinkWire />
          <EventLog
            id="authz"
            w={300}
            C={C}
            cycle={cycle}
            title="Policy decisions"
            icon={<ShieldCheck className="h-4 w-4" />}
            rows={items.map((it, k) => (
              <PolicyRow key={k} tone={it.tone} action={it.action} policy={it.policy} />
            ))}
          />
        </>
      );
    },
  },
  {
    id: "rate-limits",
    pill: "Rate limiting",
    caption: "Per-key and per-tenant limits shed excess load before it reaches a provider.",
    href: "/docs/configuration/auth#per-key-rate-limits",
    render: () => {
      const ys = providerYs(LEAN_SET.length);
      const n = 8;
      const { C } = sceneTiming(ys, n);
      const cycle = meterCycle(C, n);
      const sim = rateSchedule(C, cycle);
      return (
        <>
          <Wires ys={ys} />
          {sim.shed.map((isShed, k) =>
            isShed ? (
              <BounceDot key={k} begin={k * C} cycle={cycle} outClass="fill-orange-500" />
            ) : (
              <ForwardDot key={k} y={ys[laneOf(k, LEAN_SET.length)]} begin={k * C} cycle={cycle} />
            )
          )}
          <UserNode />
          <GatewayNode />
          <ProviderChips providers={LEAN_SET} ys={ys} />
          <SinkWire />
          <EventMeter
            w={186}
            cycle={cycle}
            icon={<Gauge className="h-4 w-4" />}
            title="Rate limit"
            keyTimes={sim.keyTimes}
            values={sim.values}
            staticFrac={0.6}
          />
        </>
      );
    },
  },
  {
    id: "guardrails",
    pill: "Guardrails",
    caption:
      "Content moderation, PII detection, and prompt-injection checks screen every request and response.",
    href: "/docs/features/guardrails",
    render: () => {
      const ys = providerYs(LEAN_SET.length);
      const items: {
        tone: "allow" | "deny" | "redact";
        label: string;
        lane?: number;
      }[] = [
        { tone: "allow", label: "passed", lane: 0 },
        { tone: "deny", label: "pii_credit_card" },
        { tone: "allow", label: "passed", lane: 2 },
        { tone: "deny", label: "prompt_attack" },
        { tone: "redact", label: "pii_email", lane: 1 },
        { tone: "allow", label: "passed", lane: 0 },
      ];
      const n = items.length;
      const { C, cycle } = sceneTiming(ys, n);
      return (
        <>
          <Wires ys={ys} />
          {items.map((it, k) => {
            if (it.tone === "deny")
              return <BounceDot key={k} begin={k * C} cycle={cycle} outClass="fill-red-500" />;
            if (it.tone === "redact")
              return (
                <ForwardDot
                  key={k}
                  y={ys[it.lane ?? 0]}
                  begin={k * C}
                  cycle={cycle}
                  outClass="fill-amber-500"
                />
              );
            return (
              <ForwardDot
                key={k}
                y={ys[it.lane ?? 0]}
                begin={k * C}
                cycle={cycle}
                outClass="fill-emerald-500"
              />
            );
          })}
          <UserNode />
          <GatewayNode />
          <ProviderChips providers={LEAN_SET} ys={ys} />
          <SinkWire />
          <EventLog
            id="guardrails"
            w={224}
            C={C}
            cycle={cycle}
            title="Guardrails"
            icon={<ShieldAlert className="h-4 w-4" />}
            href="/docs/features/guardrails"
            rows={items.map((it, k) => (
              <ScreenRow key={k} tone={it.tone} label={it.label} />
            ))}
          />
        </>
      );
    },
  },
  {
    id: "budgets",
    pill: "Budgets & cost",
    caption:
      "Scoped budgets and microcent cost tracking meter spend across orgs, teams, and projects.",
    href: "/docs/features/budgets",
    render: () => {
      const ys = providerYs(LEAN_SET.length);
      // Costs climb cumulative spend to exactly the budget by the fifth request,
      // so the bar fills before anything bounces — then, with the budget spent,
      // every remaining request bounces while the meter reads full. This matches
      // the rate-limit scene, where shedding only begins once the bar tops out.
      const costs = [1.4, 2.1, 1.8, 2.4, 2.3, 2.0, 1.5];
      const budget = 10;
      const n = costs.length;
      const { C } = sceneTiming(ys, n);
      const cycle = meterCycle(C, n);
      const sim = budgetSchedule(costs, budget, C, cycle);
      const radiusFor = (c: number) => 3.2 + (c / 2.4) * 3.6; // cost → dot size
      return (
        <>
          <Wires ys={ys} />
          {sim.shed.map((isShed, k) =>
            isShed ? (
              <BounceDot key={k} begin={k * C} cycle={cycle} outClass="fill-orange-500" />
            ) : (
              <ForwardDot
                key={k}
                y={ys[laneOf(k, LEAN_SET.length)]}
                begin={k * C}
                cycle={cycle}
                r={radiusFor(costs[k])}
              />
            )
          )}
          <UserNode />
          <GatewayNode />
          <ProviderChips providers={LEAN_SET} ys={ys} />
          <SinkWire />
          <EventMeter
            w={210}
            cycle={cycle}
            icon={<Wallet className="h-4 w-4" />}
            title="Budget"
            keyTimes={sim.keyTimes}
            values={sim.values}
            balanceSteps={sim.balanceSteps}
            staticFrac={0.77}
            staticBalance={`$7.70 / $${budget.toLocaleString("en-US")}`}
          />
        </>
      );
    },
  },
  {
    id: "caching",
    pill: "Caching",
    caption: "In-memory or Redis caching returns hits instantly, skipping the call to a provider.",
    href: "/docs/features/caching",
    render: () => {
      const ys = providerYs(4);
      const cacheY = ys[0];
      const llmYs = ys.slice(1);
      const items: { hit: boolean; lane?: number }[] = [
        { hit: true },
        { hit: false, lane: 0 },
        { hit: true },
        { hit: false, lane: 1 },
        { hit: false, lane: 2 },
        { hit: true },
      ];
      const n = items.length;
      const { C, cycle } = sceneTiming(ys, n);
      const cachePath = fullPath(cacheY);
      // Gateway → cache leg runs at 5× speed (instant hit); the user → gateway
      // leg matches every other request.
      const g = gateFrac(cachePath);
      const L = pathLength(cachePath);
      const t1 = (g * L) / SPEED;
      const t2 = ((1 - g) * L) / (5 * SPEED);
      return (
        <>
          <g fill="none" aria-hidden="true" className="stroke-fd-border" strokeWidth={1.5}>
            <path d={userPath} />
            {ys.map((y, i) => (
              <path key={i} d={providerPath(y)} />
            ))}
          </g>
          {items.map((it, k) => {
            const begin = k * C;
            if (!it.hit)
              return <ForwardDot key={k} y={llmYs[it.lane ?? 0]} begin={begin} cycle={cycle} />;
            // f1 = gateway centre (cache is checked here, so the colour changes);
            // f2 = arrival at the cache after the fast 5× hop.
            const f1 = t1 / cycle;
            const f2 = (t1 + t2) / cycle;
            const motion = (
              <animateMotion
                path={cachePath}
                dur={`${cycle}s`}
                begin={`${begin}s`}
                repeatCount="indefinite"
                calcMode="linear"
                keyPoints={`0;${g.toFixed(3)};1;1`}
                keyTimes={`0;${f1.toFixed(3)};${f2.toFixed(3)};1`}
              />
            );
            return (
              <g key={k}>
                <NodeGlow
                  x={PX}
                  y={cacheY}
                  size={56}
                  dur={cycle}
                  begin={begin}
                  at={Math.min(0.985, f2)}
                />
                {/* Primary until it passes the gateway… */}
                <circle
                  r="4.5"
                  className="fill-fd-primary motion-reduce:hidden"
                  opacity={0}
                  style={DOT_FILTER}
                >
                  {motion}
                  <animate
                    attributeName="opacity"
                    values="0;1;1;0;0"
                    keyTimes={`0;0.03;${(f1 - 0.01).toFixed(3)};${f1.toFixed(3)};1`}
                    dur={`${cycle}s`}
                    begin={`${begin}s`}
                    repeatCount="indefinite"
                  />
                </circle>
                {/* …then teal on the instant hop to the cache. */}
                <circle
                  r="4.5"
                  className="fill-teal-500 motion-reduce:hidden"
                  opacity={0}
                  style={DOT_FILTER}
                >
                  {motion}
                  <animate
                    attributeName="opacity"
                    values="0;0;1;1;0;0"
                    keyTimes={`0;${f1.toFixed(3)};${(f1 + 0.01).toFixed(3)};${(f2 - 0.02).toFixed(3)};${f2.toFixed(3)};1`}
                    dur={`${cycle}s`}
                    begin={`${begin}s`}
                    repeatCount="indefinite"
                  />
                </circle>
              </g>
            );
          })}
          <UserNode />
          <GatewayNode />
          <foreignObject
            x={PX - PROVIDER_HALF}
            y={cacheY - PROVIDER_HALF}
            width={VB_W - (PX - PROVIDER_HALF)}
            height={PROVIDER_HALF * 2}
          >
            <Link
              href="/docs/features/caching"
              aria-label="Caching documentation"
              className="group flex h-full items-center gap-3 no-underline"
            >
              <span className="flex aspect-square h-full flex-none items-center justify-center rounded-xl border border-dashed border-teal-500/60 bg-teal-500/5 shadow-sm">
                <Database className="h-5 w-5 text-teal-600 dark:text-teal-400" />
              </span>
              <span className="flex flex-col leading-tight">
                <span
                  className="font-medium text-emerald-700 dark:text-emerald-400"
                  style={{ fontSize: 14 }}
                >
                  Cache
                </span>
              </span>
            </Link>
          </foreignObject>
          {LEAN_SET.map((p, i) => (
            <Chip key={p.name} provider={p} y={llmYs[i]} />
          ))}
        </>
      );
    },
  },
  {
    id: "usage",
    pill: "Usage logging",
    caption: "Every request is logged with detailed usage metadata.",
    href: "/docs/configuration/observability",
    render: () => {
      const ys = providerYs(LEAN_SET.length);
      // provider matches the lane the dot is sent to, so the row and the dot you
      // watch arrive are the same request. `identity` is the user or service
      // account the request was tied to, which Hadrian records alongside the
      // prompt and completion counts, cost, latency, and finish reason.
      const items: {
        lane: number;
        identity: string;
        provider: string;
        tok: string;
        finish: string;
        cost: string;
        lat: string;
      }[] = [
        {
          lane: 1,
          identity: "alice@example.com",
          provider: "openai",
          tok: "1242 → 318",
          finish: "stop",
          cost: "$0.0023",
          lat: "412ms",
        },
        {
          lane: 0,
          identity: "svc-batch",
          provider: "anthropic",
          tok: "880 → 1203",
          finish: "tool_calls",
          cost: "$0.0142",
          lat: "1.2s",
        },
        {
          lane: 2,
          identity: "bob@example.com",
          provider: "gemini",
          tok: "512 → 240",
          finish: "stop",
          cost: "$0.0006",
          lat: "380ms",
        },
        {
          lane: 1,
          identity: "carol@example.com",
          provider: "openai",
          tok: "310 → 870",
          finish: "length",
          cost: "$0.0089",
          lat: "910ms",
        },
        {
          lane: 0,
          identity: "dana@example.com",
          provider: "anthropic",
          tok: "2048 → 96",
          finish: "content_filter",
          cost: "$0.0031",
          lat: "540ms",
        },
        {
          lane: 2,
          identity: "svc-ingest",
          provider: "gemini",
          tok: "1024 → 512",
          finish: "stop",
          cost: "$0.0042",
          lat: "600ms",
        },
      ];
      const n = items.length;
      const { C, cycle } = sceneTiming(ys, n);
      return (
        <>
          <Wires ys={ys} />
          {items.map((it, k) => (
            <ForwardDot key={k} y={ys[it.lane]} begin={k * C} cycle={cycle} />
          ))}
          <UserNode />
          <GatewayNode />
          <ProviderChips providers={LEAN_SET} ys={ys} />
          <SinkWire />
          <EventLog
            id="usage"
            w={480}
            C={C}
            cycle={cycle}
            title="Usage log"
            icon={<ScrollText className="h-4 w-4" />}
            rows={items.map((it, k) => (
              <UsageRow
                key={k}
                identity={it.identity}
                provider={it.provider}
                tok={it.tok}
                finish={it.finish}
                cost={it.cost}
                lat={it.lat}
              />
            ))}
          />
        </>
      );
    },
  },
  {
    id: "tools",
    pill: "Server-side tools",
    caption:
      "The gateway runs MCP tools, shell commands, file search, and web search server-side in an agentic loop with the model.",
    href: "/docs/features/agents",
    render: () => {
      const ys = providerYs(LEAN_SET.length);
      const tools = [
        { icon: <Plug className="h-4 w-4" />, label: "MCP" },
        { icon: <Terminal className="h-4 w-4" />, label: "Shell" },
        { icon: <FileSearch className="h-4 w-4" />, label: "Files" },
        { icon: <Globe className="h-4 w-4" />, label: "Web" },
      ];
      const n = 6;
      const { C, cycle } = sceneTiming(ys, n);
      const startX = GX - 165;
      const gap = 110;
      const ty = SINK_TOP + 16;
      return (
        <>
          <Wires ys={ys} />
          {/* Each request reaches the gateway, which calls a tool (down and back)
              before routing it onward — so every tool invocation has a cause. */}
          {Array.from({ length: n }, (_, k) => (
            <ForwardDot key={k} y={ys[laneOf(k, LEAN_SET.length)]} begin={k * C} cycle={cycle} />
          ))}
          <g fill="none" aria-hidden="true" className="stroke-fd-border" strokeWidth={1.5}>
            {tools.map((_, i) => (
              <path key={i} d={`M${GX},${GW_BOTTOM} L${startX + i * gap},${ty - 18}`} />
            ))}
          </g>
          <UserNode />
          <GatewayNode />
          <ProviderChips providers={LEAN_SET} ys={ys} />
          {Array.from({ length: n }, (_, k) => {
            const ti = (k * 3) % tools.length; // a different tool per request
            const tx = startX + ti * gap;
            const loop = `M${GX},${GW_BOTTOM} L${tx},${ty - 18} L${GX},${GW_BOTTOM}`;
            return (
              <Flow
                key={k}
                path={loop}
                dur={cycle}
                begin={k * C + T_GATE}
                className="fill-violet-500"
              />
            );
          })}
          {tools.map((t, i) => {
            const tx = startX + i * gap;
            return (
              <foreignObject
                key={t.label}
                x={tx - 42}
                y={ty - 16}
                width={84}
                height={34}
                aria-hidden="true"
              >
                <div className="flex h-full w-full items-center justify-center gap-1.5 rounded-lg border border-fd-border bg-fd-card text-[12px] font-medium text-fd-foreground shadow-sm">
                  <span className="text-fd-muted-foreground">{t.icon}</span>
                  {t.label}
                </div>
              </foreignObject>
            );
          })}
        </>
      );
    },
  },
];

// =====================================================================
// Tabbed, auto-cycling wrapper
// =====================================================================

const REDUCED_MOTION_QUERY = "(prefers-reduced-motion: reduce)";

function usePrefersReducedMotion() {
  return useSyncExternalStore(
    (onChange) => {
      const mq = window.matchMedia(REDUCED_MOTION_QUERY);
      mq.addEventListener("change", onChange);
      return () => mq.removeEventListener("change", onChange);
    },
    () => window.matchMedia(REDUCED_MOTION_QUERY).matches,
    () => false
  );
}

const CYCLE_MS = 6500;

// =====================================================================
// Scene picker
//
// One row of tabs that switches the diagram. Each tab is an icon + label, the
// active tab tinted with the primary colour.
// =====================================================================

const SCENE_ICONS: Record<string, React.ComponentType<{ className?: string }>> = {
  routing: Split,
  failover: Activity,
  auth: Fingerprint,
  authz: ShieldCheck,
  "rate-limits": Gauge,
  guardrails: ShieldAlert,
  budgets: Wallet,
  caching: Database,
  usage: ScrollText,
  sovereignty: Globe,
  tools: Wrench,
};

function ScenePicker({
  active,
  onSelect,
  onKeyDown,
  tablistRef,
  paused,
  onAdvance,
  pauseHandlers,
}: {
  active: number;
  onSelect: (i: number) => void;
  onKeyDown: (e: React.KeyboardEvent) => void;
  tablistRef: React.RefObject<HTMLDivElement | null>;
  // The active tab carries a progress bar that fills over one slideshow cycle and,
  // on completion, advances to the next scene — so the bar and the auto-advance are
  // the same clock. It freezes (not resets) while `paused`, preserving remaining
  // time. This is the slideshow itself, independent of the in-scene dot animation,
  // so it keeps cycling even under prefers-reduced-motion.
  paused: boolean;
  onAdvance: () => void;
  // Hovering or focusing the chips pauses the slideshow, same as the scene/caption.
  pauseHandlers: React.DOMAttributes<HTMLDivElement>;
}) {
  return (
    <div
      ref={tablistRef}
      role="tablist"
      aria-label="Gateway capabilities"
      onKeyDown={onKeyDown}
      className="flex max-w-3xl flex-wrap justify-center gap-2"
      {...pauseHandlers}
    >
      {scenes.map((scene, i) => {
        const isActive = i === active;
        const Icon = SCENE_ICONS[scene.id];
        return (
          <button
            key={scene.id}
            type="button"
            role="tab"
            aria-selected={isActive}
            aria-controls={`gw-panel-${scene.id}`}
            tabIndex={isActive ? 0 : -1}
            onClick={() => onSelect(i)}
            onFocus={() => onSelect(i)}
            className={`group relative inline-flex cursor-pointer items-center gap-1.5 rounded-lg px-3 pb-2.5 pt-1.5 text-sm font-medium transition-colors ${
              isActive
                ? "bg-fd-primary/10 text-fd-primary ring-1 ring-inset ring-fd-primary/25"
                : "text-fd-muted-foreground hover:bg-fd-muted hover:text-fd-foreground"
            }`}
          >
            {Icon && (
              <Icon
                aria-hidden="true"
                className={`h-3.5 w-3.5 shrink-0 transition-colors ${
                  isActive
                    ? "text-fd-primary"
                    : "text-fd-muted-foreground/60 group-hover:text-fd-foreground"
                }`}
              />
            )}
            <span>{scene.pill}</span>
            {isActive && (
              <span
                aria-hidden="true"
                className="pointer-events-none absolute inset-x-2 bottom-1.5 h-[3px] overflow-hidden rounded-full bg-fd-primary/20"
              >
                <span
                  onAnimationEnd={onAdvance}
                  className="block h-full w-full origin-left bg-fd-primary"
                  style={{
                    animation: `hadrian-tab-progress ${CYCLE_MS}ms linear`,
                    animationPlayState: paused ? "paused" : "running",
                  }}
                />
              </span>
            )}
          </button>
        );
      })}
    </div>
  );
}

export function GatewayDiagram() {
  const [active, setActive] = useState(0);
  const [paused, setPaused] = useState(false);
  const reducedMotion = usePrefersReducedMotion();
  // The dot/glow animation plays by default for everyone. Reduced-motion users
  // get a toggle to stop it, which falls back to the static frame.
  const [stopped, setStopped] = useState(false);
  const reduced = reducedMotion && stopped;
  const tablistRef = useRef<HTMLDivElement>(null);

  const go = useCallback(
    (i: number) => setActive(((i % scenes.length) + scenes.length) % scenes.length),
    []
  );

  const onKeyDown = (e: React.KeyboardEvent) => {
    let next: number | null = null;
    if (e.key === "ArrowRight" || e.key === "ArrowDown") next = active + 1;
    else if (e.key === "ArrowLeft" || e.key === "ArrowUp") next = active - 1;
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = scenes.length - 1;
    if (next === null) return;
    e.preventDefault();
    const idx = ((next % scenes.length) + scenes.length) % scenes.length;
    go(idx);
    // Roving tabindex: arrow keys must also move DOM focus to the activated tab,
    // otherwise focus stays on a now-tabIndex=-1 button and Tab skips the tablist.
    tablistRef.current?.querySelectorAll<HTMLElement>('[role="tab"]')[idx]?.focus();
  };

  const scene = scenes[active];

  // Pausing the slideshow is opt-in per content region — the scene, the caption,
  // and the tab chips each carry these — so the empty side gutters beside the
  // centred content (the column is full-width) never freeze the slideshow.
  const pauseHandlers = {
    onMouseEnter: () => setPaused(true),
    onMouseLeave: () => setPaused(false),
    onFocusCapture: () => setPaused(true),
    onBlurCapture: () => setPaused(false),
  };

  return (
    <ReducedMotionContext.Provider value={reduced}>
      <div className="flex flex-col items-center gap-5">
        <style>{`
          @keyframes hadrian-scene-fade { from { opacity: 0 } to { opacity: 1 } }
          @keyframes hadrian-tab-progress { from { transform: scaleX(0) } to { transform: scaleX(1) } }
          @media (prefers-reduced-motion: reduce) {
            .hadrian-force-motion .motion-reduce\\:hidden { display: revert; }
          }
        `}</style>

        <div className="w-full overflow-x-auto">
          {/* Hovering or focusing the scene pauses the slideshow and reveals the
              animation toggle (as do the caption and tab chips below). Keeping pause
              on the content regions — not the full-width column — means hovering an
              empty side gutter no longer freezes it. */}
          <div className="relative mx-auto w-full max-w-3xl sm:min-w-[720px]" {...pauseHandlers}>
            <div
              id={`gw-panel-${scene.id}`}
              role="tabpanel"
              aria-label={scene.pill}
              key={reduced ? undefined : scene.id}
              style={reduced ? undefined : { animation: "hadrian-scene-fade 420ms ease" }}
            >
              <svg
                viewBox={`0 ${-VB_TOP_PAD} ${VB_W} ${VB_H + VB_TOP_PAD}`}
                aria-label={`Hadrian Gateway, ${scene.pill}. ${scene.caption}`}
                className={`h-auto w-full${reduced ? "" : " hadrian-force-motion"}`}
              >
                <defs>
                  <filter id="hadrian-dot-glow" x="-200%" y="-200%" width="500%" height="500%">
                    <feGaussianBlur stdDeviation="2.5" result="blur" />
                    <feMerge>
                      <feMergeNode in="blur" />
                      <feMergeNode in="SourceGraphic" />
                    </feMerge>
                  </filter>
                  <filter id="hadrian-node-glow" x="-100%" y="-100%" width="300%" height="300%">
                    <feGaussianBlur stdDeviation="7" />
                  </filter>
                </defs>
                {scene.render()}
              </svg>
            </div>
            {/* The animation plays by default; reduced-motion users get a toggle to
              stop it and fall back to the static frame. Like the "Slideshow paused"
              badge, it surfaces on hover/focus so it doesn't sit on the scene. */}
            {reducedMotion && (
              <button
                type="button"
                onClick={() => setStopped((s) => !s)}
                aria-label={stopped ? "Play animation" : "Stop animation"}
                className={`absolute left-2 top-2 z-10 flex items-center gap-1 rounded-full border border-fd-border bg-fd-card/90 px-2 py-1 text-[11px] font-medium text-fd-muted-foreground shadow-sm backdrop-blur transition duration-300 hover:border-fd-primary/60 hover:text-fd-primary ${
                  paused ? "opacity-100" : "pointer-events-none opacity-0"
                }`}
              >
                {stopped ? (
                  <Play className="h-3 w-3" aria-hidden="true" />
                ) : (
                  <Square className="h-3 w-3" aria-hidden="true" />
                )}
                {stopped ? "Play animation" : "Stop animation"}
              </button>
            )}
            {/* Hovering or focusing the diagram pauses the slideshow; this badge
                makes that explicit so the frozen progress bar isn't read as a stall. */}
            <div
              aria-hidden="true"
              className={`pointer-events-none absolute right-2 top-2 z-10 flex items-center gap-1 rounded-full border border-fd-border bg-fd-card/90 px-2 py-1 text-[11px] font-medium text-fd-muted-foreground shadow-sm backdrop-blur transition-opacity duration-300 ${
                paused ? "opacity-100" : "opacity-0"
              }`}
            >
              <Pause className="h-3 w-3" aria-hidden="true" />
              Slideshow paused. Move away to resume.
            </div>
          </div>
        </div>

        <div
          className="flex min-h-[4.25rem] max-w-2xl flex-col items-center justify-center gap-1.5 text-center text-sm text-fd-muted-foreground"
          {...pauseHandlers}
        >
          <p>{scene.caption}</p>
          <Link href={scene.href} className="whitespace-nowrap font-medium text-fd-primary">
            Learn more →
          </Link>
        </div>

        {/* Tabs switch the diagram (they do not navigate away). */}
        <ScenePicker
          active={active}
          onSelect={setActive}
          onKeyDown={onKeyDown}
          tablistRef={tablistRef}
          paused={paused}
          onAdvance={() => go(active + 1)}
          pauseHandlers={pauseHandlers}
        />
      </div>
    </ReducedMotionContext.Provider>
  );
}
