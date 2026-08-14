"""The single-page app served by :mod:`diskanalyzer.server`.

Why a sunburst and not the treemap
----------------------------------
The static report draws a treemap, which is the right shape for "how big is
this compared to that": rectangles sit side by side and area is easy to
compare. It is the wrong shape for walking down a path, because every step
throws the previous picture away and redraws from scratch, so you lose your
place.

A sunburst keeps the whole descent on screen at once. The ring you are looking
at stays in the middle and its subfolders fan outward, so five levels of a
drive are one picture rather than five clicks with nothing connecting them.
That is why DaisyDisk uses one, and it is the piece the terminal and the static
report could not give you.

Everything is inline, as in the report: one document, no build step, no asset
to lose. The page talks to the local API for one folder at a time instead of
carrying the whole drive in a script tag.
"""

from __future__ import annotations

import json

from . import categories


def render(token: str) -> str:
    """The app, with this session's token and the shared palette baked in."""
    meta = {
        category.key: {
            "label": category.label,
            "confidence": category.confidence,
            "hint": category.hint,
        }
        for category in categories.CATEGORIES
    }
    return (
        TEMPLATE.replace("__PALETTE__", json.dumps(categories.COLORS))
        .replace("__CATMETA__", json.dumps(meta, ensure_ascii=False))
        .replace("__TOKEN__", json.dumps(token))
    )


TEMPLATE = r"""<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>DiskAnalyzer</title>
<style>
:root{
  --bg:#f6f7f9; --panel:#ffffff; --ink:#14181f; --muted:#697386;
  --line:#e3e7ed; --accent:#2f6fed; --safe:#1a7f4b; --review:#a86a00; --info:#697386;
  --shadow:0 1px 2px rgba(16,24,40,.06),0 8px 24px rgba(16,24,40,.06);
  --hole:#ffffff;
}
@media (prefers-color-scheme:dark){
  :root{
    --bg:#0e1116; --panel:#161b22; --ink:#e8edf4; --muted:#8b97a8;
    --line:#242c37; --accent:#5b8cff; --safe:#4bd08b; --review:#e0a33e; --info:#8b97a8;
    --shadow:0 1px 2px rgba(0,0,0,.3),0 8px 24px rgba(0,0,0,.25);
    --hole:#161b22;
  }
}
*{box-sizing:border-box}
html,body{height:100%}
body{margin:0;background:var(--bg);color:var(--ink);overflow:hidden;
  font:14px/1.55 ui-sans-serif,system-ui,"Segoe UI",Roboto,sans-serif}
button{font:inherit;color:inherit}
.mono{font-family:ui-monospace,"Cascadia Code",Consolas,monospace}

/* ---------------------------------------------------------------- shell */
.app{display:flex;flex-direction:column;height:100%}
.topbar{display:flex;align-items:center;gap:14px;padding:11px 18px;
  border-bottom:1px solid var(--line);background:var(--panel);flex:none}
.brand{font-weight:680;letter-spacing:-.02em;font-size:15px;white-space:nowrap}
.badge{padding:2px 9px;border-radius:999px;font-size:11.5px;font-weight:640;
  background:color-mix(in srgb,var(--safe) 16%,transparent);color:var(--safe);white-space:nowrap}
.pills{display:flex;gap:16px;margin-left:8px;overflow:hidden}
.pill{white-space:nowrap;font-size:12px;color:var(--muted)}
.pill b{display:block;font-size:14px;color:var(--ink);font-weight:640;
  font-variant-numeric:tabular-nums}
.pill.safe b{color:var(--safe)} .pill.review b{color:var(--review)}
.spacer{flex:1}
.btn{border:1px solid var(--line);background:var(--bg);border-radius:9px;
  padding:7px 13px;cursor:pointer;white-space:nowrap;transition:background .12s,border-color .12s}
.btn:hover{border-color:var(--accent)}
.btn.primary{background:var(--accent);border-color:var(--accent);color:#fff;font-weight:620}
.btn.primary:hover{filter:brightness(1.08)}
.btn:disabled{opacity:.45;cursor:not-allowed}
.btn.ghost{background:none}

/* --------------------------------------------------------------- setup */
.screen{flex:1;min-height:0;overflow:auto}
.setup{max-width:940px;margin:0 auto;padding:38px 22px 60px}
h1{font-size:26px;margin:0 0 6px;letter-spacing:-.025em}
.lede{color:var(--muted);margin:0 0 30px;max-width:64ch}
h2{font-size:12px;margin:30px 0 12px;letter-spacing:.07em;text-transform:uppercase;color:var(--muted)}
.drives{display:grid;grid-template-columns:repeat(auto-fill,minmax(215px,1fr));gap:13px}
.drive{text-align:left;background:var(--panel);border:1px solid var(--line);border-radius:13px;
  padding:15px 16px;cursor:pointer;box-shadow:var(--shadow);transition:border-color .12s,transform .12s}
.drive:hover{transform:translateY(-1px)}
.drive[aria-pressed=true]{border-color:var(--accent);
  box-shadow:0 0 0 1px var(--accent),var(--shadow)}
.drive .hd{display:flex;align-items:baseline;gap:8px}
.drive .lt{font-size:20px;font-weight:700;letter-spacing:-.02em}
.drive .lb{color:var(--muted);font-size:12.5px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.drive .fr{margin-top:9px;font-size:12.5px;color:var(--muted);font-variant-numeric:tabular-nums}
.gauge{height:7px;border-radius:999px;background:var(--line);overflow:hidden;margin-top:8px}
.gauge>i{display:block;height:100%;background:var(--accent);transition:width .3s}
.gauge.tight>i{background:var(--review)}
.row{display:flex;gap:9px;align-items:center;flex-wrap:wrap}
input[type=text]{flex:1;min-width:240px;padding:9px 12px;border-radius:9px;border:1px solid var(--line);
  background:var(--panel);color:var(--ink);font:inherit}
input[type=text]:focus{outline:none;border-color:var(--accent)}
.chips{display:flex;flex-wrap:wrap;gap:7px;margin-top:12px}
.chip{display:flex;align-items:center;gap:7px;background:var(--panel);border:1px solid var(--line);
  border-radius:999px;padding:5px 7px 5px 12px;font-size:12.5px}
.chip button{border:0;background:none;cursor:pointer;color:var(--muted);
  padding:0 4px;border-radius:50%;font-size:15px;line-height:1}
.chip button:hover{color:var(--ink)}
.opts{display:flex;flex-direction:column;gap:11px;margin-top:6px}
.opt{display:flex;gap:10px;align-items:flex-start;cursor:pointer;max-width:70ch}
.opt input{margin:3px 0 0}
.opt .t{font-weight:600;display:block}
.opt .d{color:var(--muted);font-size:12.5px;display:block}
.go{margin-top:30px;display:flex;gap:12px;align-items:center}
.note{color:var(--muted);font-size:12.5px}

/* ------------------------------------------------------------ scanning */
#screen-scanning{display:flex;align-items:center;justify-content:center}
.scanning{width:100%;max-width:660px;padding:22px}
.counter{font-size:44px;font-weight:680;letter-spacing:-.03em;font-variant-numeric:tabular-nums}
.sub{color:var(--muted);margin-top:4px}
.sweep{height:5px;border-radius:999px;background:var(--line);overflow:hidden;margin:26px 0 12px}
.sweep>i{display:block;height:100%;width:34%;border-radius:999px;background:var(--accent);
  animation:sweep 1.25s ease-in-out infinite}
/* Ends where a 34%-wide bar just clears the right edge. Travelling further only
   buys an empty track, and an empty track reads as a stall. */
@keyframes sweep{0%{transform:translateX(-100%)}100%{transform:translateX(295%)}}
.cur{color:var(--muted);font-size:12.5px;height:1.5em;overflow:hidden;
  text-overflow:ellipsis;white-space:nowrap;direction:rtl;text-align:left}

/* ----------------------------------------------------------------- map */
.strip{display:flex;height:5px;flex:none;background:var(--line)}
.strip>i{height:100%}
.mapwrap{flex:1;min-height:0;display:grid;grid-template-columns:1fr 372px}
/* Narrow: stack instead of hiding the panel. The list is the half that still
   works at any size, so dropping it would be the wrong economy; the rings get
   a floor of 280px and the list takes what is left. */
@media (max-width:1000px){
  .mapwrap{grid-template-columns:1fr;grid-template-rows:minmax(280px,48%) 1fr}
  .side{border-left:0;border-top:1px solid var(--line)}
}
/* min-height:0 on both axes: a grid item refuses to shrink below its content
   by default, which would let the rings push the legend off the window. */
.stage{position:relative;min-width:0;min-height:0;display:flex;flex-direction:column;
  padding:12px 6px 6px}
.crumbs{display:flex;gap:2px;align-items:center;flex-wrap:wrap;padding:0 12px 8px;
  font-size:12.5px;flex:none}
.crumbs button{background:none;border:0;cursor:pointer;color:var(--accent);
  padding:3px 6px;border-radius:7px;max-width:230px;overflow:hidden;
  text-overflow:ellipsis;white-space:nowrap}
.crumbs button:hover{background:color-mix(in srgb,var(--accent) 12%,transparent)}
.crumbs button:last-child{color:var(--ink);font-weight:620}
.crumbs .sep{color:var(--muted)}
.canvas{flex:1;min-height:0;position:relative}
/* Taken out of flow on purpose. An inline SVG with a square viewBox carries an
   intrinsic aspect ratio, so at width:100% it demands a matching height and
   pushes itself past the bottom of the window however the flexbox is set up.
   Pinned to the box instead, it scales to whatever space is actually left. */
svg#burst{position:absolute;inset:0;width:100%;height:100%;display:block;touch-action:none}
svg#burst path{cursor:pointer;transition:opacity .1s}
svg#burst path.flat{cursor:default}
svg#burst.dim path{opacity:.4}
svg#burst.dim path.lit{opacity:1}
/* Width is set from JS, not CSS: the rings letterbox to whichever side of the
   canvas is shorter, so a percentage of the width would let this label spill
   across them the moment the window is wider than it is tall. */
.hole{position:absolute;left:50%;top:50%;transform:translate(-50%,-50%);
  text-align:center;pointer-events:none}
.hole .up{font-size:11.5px;color:var(--muted);letter-spacing:.04em;text-transform:uppercase}
.hole .nm{font-weight:660;letter-spacing:-.02em;margin:3px 0;overflow:hidden;
  text-overflow:ellipsis;white-space:nowrap}
.hole .sz{font-size:21px;font-weight:680;letter-spacing:-.02em;font-variant-numeric:tabular-nums}
.hole .ct{font-size:12px;color:var(--muted)}
.tip{position:fixed;z-index:20;pointer-events:none;background:var(--panel);
  border:1px solid var(--line);border-radius:9px;padding:8px 11px;box-shadow:var(--shadow);
  max-width:340px;font-size:12.5px;opacity:0;transition:opacity .1s}
.tip.on{opacity:1}
.tip .nm{font-weight:640;word-break:break-all}
.tip .mt{color:var(--muted);margin-top:2px;font-variant-numeric:tabular-nums}
.legend{display:flex;flex-wrap:wrap;gap:4px 14px;padding:8px 14px 4px;flex:none;
  font-size:11.5px;color:var(--muted)}
.legend span{display:flex;align-items:center;gap:5px}
.legend i{width:9px;height:9px;border-radius:3px}

/* ---------------------------------------------------------------- side */
.side{border-left:1px solid var(--line);background:var(--panel);display:flex;
  flex-direction:column;min-height:0}
.sidehd{padding:13px 15px 10px;border-bottom:1px solid var(--line);flex:none}
.sidehd .k{font-size:11px;text-transform:uppercase;letter-spacing:.07em;color:var(--muted)}
.sidehd .v{font-weight:640;margin-top:3px;word-break:break-all;font-size:13px}
.sidehd .m{color:var(--muted);font-size:12.5px;margin-top:3px;font-variant-numeric:tabular-nums}
.tag{display:inline-block;padding:1px 7px;border-radius:6px;font-size:11px;font-weight:620;margin-top:7px}
.tag.safe{background:color-mix(in srgb,var(--safe) 16%,transparent);color:var(--safe)}
.tag.review{background:color-mix(in srgb,var(--review) 18%,transparent);color:var(--review)}
.tag.info{background:color-mix(in srgb,var(--info) 16%,transparent);color:var(--info)}
.hint{color:var(--muted);font-size:12px;margin-top:6px}
.list{flex:1;min-height:0;overflow:auto;padding:6px 0 18px}
.item{display:grid;grid-template-columns:11px 1fr auto;gap:9px;align-items:center;
  padding:7px 15px;cursor:pointer;border:0;background:none;width:100%;text-align:left}
.item:hover,.item.lit{background:color-mix(in srgb,var(--accent) 8%,transparent)}
.item i{width:11px;height:11px;border-radius:3px}
.item .nm{overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-size:13px}
.item .sz{font-size:12.5px;font-variant-numeric:tabular-nums;color:var(--muted);white-space:nowrap}
.item .bar{grid-column:2/4;height:3px;border-radius:999px;background:var(--line);overflow:hidden}
.item .bar>b{display:block;height:100%}
.item.rest .nm{font-style:italic;color:var(--muted)}
.sideft{padding:11px 15px;border-top:1px solid var(--line);flex:none;display:flex;gap:8px}
.empty{color:var(--muted);font-style:italic;padding:18px 15px}

/* --------------------------------------------------------------- toast */
/* Bottom right, not bottom centre: centred toasts land straight on top of the
   legend, and the legend is the key to the colours you are looking at. */
.toasts{position:fixed;right:24px;bottom:22px;z-index:40;
  display:flex;flex-direction:column;gap:8px;align-items:flex-end}
.toast{background:var(--panel);border:1px solid var(--line);border-radius:10px;
  padding:10px 15px;box-shadow:var(--shadow);max-width:70vw}
.toast.bad{border-color:var(--review)}
.toast .p{color:var(--muted);font-size:12px;word-break:break-all;margin-top:3px}
[hidden]{display:none !important}
</style>
</head>
<body>
<div class="app">

  <header class="topbar">
    <span class="brand">DiskAnalyzer</span>
    <span class="badge" title="This tool has no code path that deletes, moves or modifies a file.">read-only</span>
    <div class="pills" id="pills"></div>
    <span class="spacer"></span>
    <button class="btn ghost" id="btn-mode" hidden>Colour: category</button>
    <button class="btn" id="btn-export" hidden>Save report</button>
    <button class="btn" id="btn-new" hidden>New scan</button>
  </header>

  <!-- ------------------------------------------------------------ setup -->
  <section class="screen" id="screen-setup">
    <div class="setup">
      <h1>What is using your space?</h1>
      <p class="lede">Pick a drive or a folder. DiskAnalyzer walks it once, then draws
        every folder as a ring you can click into. It only ever reads: nothing is
        deleted, moved or modified, and it checks its own source for the ability to
        do so before each scan.</p>

      <h2>Drives</h2>
      <div class="drives" id="drives"><p class="note">Looking for drives...</p></div>

      <h2>Or a specific folder</h2>
      <div class="row">
        <input type="text" id="custom" placeholder="C:\Users\you\Projects"
               autocomplete="off" spellcheck="false">
        <button class="btn" id="btn-add">Add</button>
      </div>
      <div class="chips" id="chips"></div>

      <h2>Options</h2>
      <div class="opts">
        <label class="opt">
          <input type="checkbox" id="opt-os">
          <span><span class="t">Include Windows itself</span>
            <span class="d">Off by default. The live OS is not something you can
              reclaim, and skipping it makes the scan faster and the picture
              actionable.</span></span>
        </label>
        <label class="opt">
          <input type="checkbox" id="opt-alloc">
          <span><span class="t">Show space on disk</span>
            <span class="d">Rounds every file up to whole clusters, which is closer to
              what the drive reports for folders full of tiny files.</span></span>
        </label>
        <label class="opt">
          <input type="checkbox" id="opt-dups">
          <span><span class="t">Look for duplicates</span>
            <span class="d">Compares size plus the first and last 64 KB of same-sized
              files. Adds a little reading after the walk.</span></span>
        </label>
      </div>

      <div class="go">
        <button class="btn primary" id="btn-scan" disabled>Analyze</button>
        <span class="note" id="pick-note">Choose at least one drive or folder.</span>
      </div>
    </div>
  </section>

  <!-- --------------------------------------------------------- scanning -->
  <section class="screen" id="screen-scanning" hidden>
    <div class="scanning">
      <div class="counter" id="sc-size">0 B</div>
      <div class="sub" id="sc-files">counting files...</div>
      <div class="sweep"><i></i></div>
      <div class="cur mono" id="sc-cur"></div>
      <div class="go">
        <button class="btn" id="btn-cancel">Cancel</button>
        <span class="note" id="sc-note">Reading only. Nothing is being changed.</span>
      </div>
    </div>
  </section>

  <!-- -------------------------------------------------------------- map -->
  <section class="screen" id="screen-map" hidden style="display:flex;flex-direction:column">
    <div class="strip" id="strip"></div>
    <div class="mapwrap">
      <div class="stage">
        <nav class="crumbs" id="crumbs"></nav>
        <div class="canvas">
          <svg id="burst" viewBox="0 0 1000 1000" preserveAspectRatio="xMidYMid meet"
               role="img" aria-label="Folder sizes as concentric rings"></svg>
          <div class="hole" id="hole"></div>
        </div>
        <div class="legend" id="legend"></div>
      </div>
      <aside class="side">
        <div class="sidehd" id="sidehd"></div>
        <div class="list" id="list"></div>
        <div class="sideft">
          <button class="btn" id="btn-reveal" style="flex:1">Reveal in Explorer</button>
        </div>
      </aside>
    </div>
  </section>
</div>

<div class="tip" id="tip"></div>
<div class="toasts" id="toasts"></div>

<script>
"use strict";
const TOKEN = __TOKEN__;
const PALETTE = __PALETTE__;
const CATMETA = __CATMETA__;

/* The token stays in the address bar on purpose. It is a loopback URL and the
   page sends no referrer, so the cost is nil, and keeping it there means a
   plain browser refresh still works instead of locking you out of your scan. */

/* ------------------------------------------------------------- helpers */
const $ = id => document.getElementById(id);
const TAU = Math.PI * 2;

function fmtSize(n){
  if(n === null || n === undefined) return "";
  const u = ["B","KB","MB","GB","TB","PB"];
  let v = n, i = 0;
  while(Math.abs(v) >= 1024 && i < u.length - 1){ v /= 1024; i++; }
  return i === 0 ? Math.round(v) + " B" : v.toFixed(1) + " " + u[i];
}
const fmtNum = n => (n || 0).toLocaleString();
function fmtDur(s){
  s = Math.round(s || 0);
  return s < 60 ? s + "s" : Math.floor(s / 60) + "m " + String(s % 60).padStart(2, "0") + "s";
}
function esc(s){
  return String(s).replace(/[&<>"']/g, c =>
    ({"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;","'":"&#39;"}[c]));
}
const colorOf = k => PALETTE[k] || PALETTE.other;

/* Lighten toward the panel colour so deeper rings recede without losing the
   category hue that gives them meaning. */
function lighten(hex, amount){
  const n = parseInt(hex.slice(1), 16);
  const mix = c => Math.round(c + (255 - c) * amount);
  return "rgb(" + mix((n >> 16) & 255) + "," + mix((n >> 8) & 255) + "," + mix(n & 255) + ")";
}
const RAINBOW = ["#2f6fed","#0891b2","#14b8a6","#22c55e","#84cc16","#eab308",
                 "#f97316","#ef4444","#ec4899","#a855f7","#6366f1","#0ea5e9"];

async function api(path, opts){
  const init = Object.assign({}, opts);
  init.headers = Object.assign({"X-DiskAnalyzer-Token": TOKEN}, init.headers || {});
  if(init.body){
    init.headers["Content-Type"] = "application/json";
    init.body = JSON.stringify(init.body);
    init.method = init.method || "POST";
  }
  const res = await fetch(path, init);
  const data = await res.json().catch(() => ({}));
  if(!res.ok) throw new Error(data.error || ("request failed: " + res.status));
  return data;
}

function toast(text, detail, bad){
  const box = document.createElement("div");
  box.className = "toast" + (bad ? " bad" : "");
  box.innerHTML = esc(text) + (detail ? '<div class="p">' + esc(detail) + "</div>" : "");
  $("toasts").appendChild(box);
  setTimeout(() => box.remove(), 6000);
}

/* --------------------------------------------------------------- state */
const S = {
  volumes: [], picked: new Set(), extra: [],
  node: null, summary: null, sel: null, mode: "category",
  poll: null, segs: [], cancelling: false
};

function screen(name){
  for(const key of ["setup","scanning","map"]) $("screen-" + key).hidden = key !== name;
  const onMap = name === "map";
  $("btn-export").hidden = !onMap;
  $("btn-new").hidden = !onMap;
  $("btn-mode").hidden = !onMap;
}

/* --------------------------------------------------------------- setup */
function targets(){
  return S.volumes.filter(v => S.picked.has(v.root)).map(v => v.root).concat(S.extra);
}
function refreshGo(){
  const n = targets().length;
  $("btn-scan").disabled = n === 0;
  $("pick-note").textContent = n === 0
    ? "Choose at least one drive or folder."
    : n + (n === 1 ? " target selected." : " targets selected.");
}

async function loadVolumes(){
  let data;
  try{ data = await api("/api/volumes"); }
  catch(err){ $("drives").innerHTML = '<p class="note">' + esc(err.message) + "</p>"; return; }

  S.volumes = data.volumes || [];
  const host = $("drives");
  host.innerHTML = "";
  if(!S.volumes.length){ host.innerHTML = '<p class="note">No drives found.</p>'; return; }

  for(const v of S.volumes){
    const used = v.total ? v.used / v.total : 0;
    const card = document.createElement("button");
    card.className = "drive";
    card.type = "button";
    card.setAttribute("aria-pressed", "false");
    card.innerHTML =
      '<div class="hd"><span class="lt">' + esc(v.letter || v.root) + '</span>' +
      '<span class="lb">' + esc(v.label || v.kind) + "</span></div>" +
      '<div class="gauge' + (used > 0.9 ? " tight" : "") + '"><i style="width:' +
        (used * 100).toFixed(1) + '%"></i></div>' +
      '<div class="fr">' + fmtSize(v.free) + " free of " + fmtSize(v.total) + "</div>";
    card.onclick = () => {
      if(S.picked.has(v.root)) S.picked.delete(v.root); else S.picked.add(v.root);
      card.setAttribute("aria-pressed", String(S.picked.has(v.root)));
      refreshGo();
    };
    host.appendChild(card);
  }

  const system = S.volumes.find(v => v.letter === "C") || S.volumes[0];
  if(system){ S.picked.add(system.root); host.firstChild.setAttribute("aria-pressed", "true"); }
  refreshGo();
}

function renderChips(){
  const host = $("chips");
  host.innerHTML = "";
  S.extra.forEach((path, idx) => {
    const chip = document.createElement("span");
    chip.className = "chip";
    chip.innerHTML = '<span class="mono">' + esc(path) + "</span>";
    const kill = document.createElement("button");
    kill.type = "button";
    kill.title = "Remove";
    kill.textContent = "\u00d7";
    kill.onclick = () => { S.extra.splice(idx, 1); renderChips(); refreshGo(); };
    chip.appendChild(kill);
    host.appendChild(chip);
  });
  refreshGo();
}

function addCustom(){
  const box = $("custom");
  const value = box.value.trim();
  if(!value) return;
  if(!S.extra.includes(value)) S.extra.push(value);
  box.value = "";
  renderChips();
}

/* ------------------------------------------------------------ scanning */
async function startScan(){
  const body = {
    roots: targets(),
    includeOs: $("opt-os").checked,
    allocated: $("opt-alloc").checked,
    duplicates: $("opt-dups").checked
  };
  try{ await api("/api/scan", {body}); }
  catch(err){ toast("Could not start the scan", err.message, true); return; }
  screen("scanning");
  S.cancelling = false;
  $("btn-cancel").disabled = false;
  $("btn-cancel").textContent = "Cancel";
  $("sc-size").textContent = "0 B";
  $("sc-files").textContent = "counting files...";
  $("sc-cur").textContent = "";
  poll();
}

function poll(){
  clearTimeout(S.poll);
  S.poll = setTimeout(tick, 250);
}

async function tick(){
  let st;
  try{ st = await api("/api/status"); }
  catch(err){ toast("Lost contact with DiskAnalyzer", err.message, true); return; }

  if(st.phase === "scanning" || st.phase === "analyzing"){
    $("sc-size").textContent = fmtSize(st.size);
    $("sc-files").textContent = st.phase === "analyzing"
      ? "sorting " + fmtNum(st.files) + " files..."
      : fmtNum(st.files) + " files in " + fmtDur(st.elapsed);
    $("sc-cur").textContent = st.phase === "analyzing" ? "" : st.current;
    /* The walk only checks for a cancel every few thousand files, so the click
       and the stop are not the same instant. Without S.cancelling the next poll
       would put the button back and invite a second, pointless click. */
    $("btn-cancel").disabled = S.cancelling || st.phase === "analyzing";
    poll();
    return;
  }

  if(st.phase === "ready"){ await enterMap(st); return; }
  if(st.phase === "cancelled"){ screen("setup"); toast(st.message || "Scan cancelled."); return; }
  if(st.phase === "error"){ screen("setup"); toast("Scan failed", st.message, true); return; }
  screen("setup");
}

/* ----------------------------------------------------------- summaries */
function renderPills(){
  const s = S.summary;
  if(!s){ $("pills").innerHTML = ""; return; }
  const vol = (s.volumes || [])[0];
  const bits = [];
  if(vol) bits.push('<span class="pill">free on ' + esc(vol.root) +
    "<b>" + fmtSize(vol.free) + "</b></span>");
  bits.push('<span class="pill">scanned<b>' + fmtSize(s.scan.size) + "</b></span>");
  bits.push('<span class="pill safe">regenerable<b>' + fmtSize(s.reclaim.safe || 0) + "</b></span>");
  bits.push('<span class="pill review">worth review<b>' + fmtSize(s.reclaim.review || 0) + "</b></span>");
  if(s.reclaim.duplicates)
    bits.push('<span class="pill">duplicated<b>' + fmtSize(s.reclaim.duplicates) + "</b></span>");
  $("pills").innerHTML = bits.join("");
}

function renderStrip(){
  const s = S.summary;
  const host = $("strip");
  host.innerHTML = "";
  if(!s) return;
  for(const c of s.categories){
    if(c.share < 0.002) continue;
    const cell = document.createElement("i");
    cell.style.width = (c.share * 100).toFixed(2) + "%";
    cell.style.background = colorOf(c.key);
    cell.title = c.label + " - " + fmtSize(c.size) + " (" + (c.share * 100).toFixed(1) + "%)";
    host.appendChild(cell);
  }
}

function renderLegend(){
  const s = S.summary;
  const host = $("legend");
  host.innerHTML = "";
  if(!s || S.mode !== "category") return;
  for(const c of s.categories.slice(0, 12)){
    if(c.share < 0.005) continue;
    const item = document.createElement("span");
    item.innerHTML = '<i style="background:' + colorOf(c.key) + '"></i>' +
      esc(c.label) + " " + fmtSize(c.size);
    host.appendChild(item);
  }
}

/* --------------------------------------------------------------- rings */
const CX = 500, CY = 500, R0 = 158, RING = 62, RINGS = 5;
const MIN_SWEEP = 0.005;   /* below a third of a degree a slice is sub-pixel */

function polar(r, a){ return [CX + r * Math.cos(a), CY + r * Math.sin(a)]; }

function arcPath(a0, a1, ri, ro){
  /* A slice covering the whole circle would start and end at the same point,
     which SVG draws as nothing at all. Leaving a hair of a gap keeps it. */
  if(a1 - a0 > TAU - 1e-6) a1 = a0 + TAU - 1e-6;
  const laf = (a1 - a0) > Math.PI ? 1 : 0;
  const [x0, y0] = polar(ro, a0), [x1, y1] = polar(ro, a1);
  const [x2, y2] = polar(ri, a1), [x3, y3] = polar(ri, a0);
  return "M" + x0 + " " + y0 + "A" + ro + " " + ro + " 0 " + laf + " 1 " + x1 + " " + y1 +
         "L" + x2 + " " + y2 + "A" + ri + " " + ri + " 0 " + laf + " 0 " + x3 + " " + y3 + "Z";
}

function layout(root){
  const segs = [];
  (function walk(item, depth, a0, a1, tint){
    const kids = item.children || [];
    const total = item.size || 1;
    let a = a0;
    kids.forEach((kid, idx) => {
      const sweep = (a1 - a0) * (kid.size / total);
      if(sweep >= MIN_SWEEP){
        const hue = depth === 0 ? RAINBOW[idx % RAINBOW.length] : tint;
        segs.push({d: kid, depth, a0: a, a1: a + sweep, hue});
        if(depth + 1 < RINGS && kid.kind === "dir") walk(kid, depth + 1, a, a + sweep, hue);
      }
      a += sweep;
    });
  })(root, 0, -Math.PI / 2, -Math.PI / 2 + TAU, RAINBOW[0]);
  return segs;
}

function fillFor(seg){
  const base = S.mode === "category" ? colorOf(seg.d.cat) : seg.hue;
  if(seg.d.kind === "rest") return lighten(base, 0.62);
  /* Alternating a few percent keeps neighbouring slices of the same category
     apart, which the stroke alone cannot do on a thin ring. */
  const stagger = (seg.depth % 2) * 0.03;
  return lighten(base, Math.min(0.55, seg.depth * 0.1 + stagger));
}

function drawMap(){
  const svg = $("burst");
  while(svg.firstChild) svg.removeChild(svg.firstChild);
  S.segs = layout(S.node);

  const NS = "http://www.w3.org/2000/svg";
  const hit = document.createElementNS(NS, "circle");
  hit.setAttribute("cx", CX); hit.setAttribute("cy", CY); hit.setAttribute("r", R0 - 4);
  hit.setAttribute("fill", "var(--hole)");
  hit.style.cursor = S.node.parent ? "pointer" : "default";
  if(S.node.parent) hit.addEventListener("click", () => openNode(S.node.parent));
  svg.appendChild(hit);

  for(const seg of S.segs){
    const ri = R0 + seg.depth * RING, ro = ri + RING - 2;
    const path = document.createElementNS(NS, "path");
    path.setAttribute("d", arcPath(seg.a0, seg.a1, ri, ro));
    path.setAttribute("fill", fillFor(seg));
    path.setAttribute("stroke", "var(--hole)");
    path.setAttribute("stroke-width", "1.2");
    const drillable = seg.d.kind === "dir" && (seg.d.children || seg.d.more);
    if(!drillable) path.classList.add("flat");
    path.addEventListener("mouseenter", ev => hover(seg, ev));
    path.addEventListener("mousemove", moveTip);
    path.addEventListener("mouseleave", unhover);
    path.addEventListener("click", () => {
      select(seg.d);
      if(drillable) openNode(seg.d.path);
    });
    seg.el = path;
    svg.appendChild(path);
  }

  const node = S.node;
  const parent = node.crumbs.length > 1 ? node.crumbs[node.crumbs.length - 2].name : "";
  $("hole").innerHTML =
    (node.parent ? '<div class="up">&uarr; ' + esc(parent) + "</div>" : "") +
    '<div class="nm" title="' + esc(node.path) + '">' + esc(node.name) + "</div>" +
    '<div class="sz">' + fmtSize(node.size) + "</div>" +
    '<div class="ct">' + fmtNum(node.files) + " files</div>";
  sizeHole();
}

/* Match the label to the drawn hole rather than to the box around it. */
function sizeHole(){
  const box = document.querySelector(".canvas").getBoundingClientRect();
  const diameter = Math.min(box.width, box.height) * (R0 * 2 / 1000);
  const hole = $("hole");
  hole.style.width = Math.max(0, diameter - 22) + "px";
  /* Under about 110px the three lines cannot be read anyway, and a label
     nobody can read is worse than rings with nothing in the middle. */
  hole.hidden = diameter < 110;
}

/* ---------------------------------------------------------- highlights */
function hover(seg, ev){
  const tip = $("tip");
  const share = S.node.size ? (seg.d.size / S.node.size) * 100 : 0;
  const meta = CATMETA[seg.d.cat];
  tip.innerHTML = '<div class="nm">' + esc(seg.d.name) + "</div>" +
    '<div class="mt">' + fmtSize(seg.d.size) + " &middot; " + share.toFixed(1) + "% of view" +
    (seg.d.files ? " &middot; " + fmtNum(seg.d.files) + " files" : "") + "</div>" +
    (meta && seg.d.kind !== "rest" ? '<div class="mt">' + esc(meta.label) + "</div>" : "");
  tip.classList.add("on");
  moveTip(ev);
  $("burst").classList.add("dim");
  seg.el.classList.add("lit");
  markRow(seg.d.path, true);
}

function moveTip(ev){
  const tip = $("tip");
  const pad = 16;
  let x = ev.clientX + pad, y = ev.clientY + pad;
  const box = tip.getBoundingClientRect();
  if(x + box.width > innerWidth - 8) x = ev.clientX - box.width - pad;
  if(y + box.height > innerHeight - 8) y = ev.clientY - box.height - pad;
  tip.style.left = x + "px";
  tip.style.top = y + "px";
}

function unhover(){
  $("tip").classList.remove("on");
  $("burst").classList.remove("dim");
  for(const seg of S.segs) seg.el.classList.remove("lit");
  markRow(null, false);
}

function markRow(path, on){
  for(const row of $("list").children){
    row.classList.toggle("lit", Boolean(on) && row.dataset.path === path);
  }
}

function litFromRow(path, on){
  $("burst").classList.toggle("dim", on);
  for(const seg of S.segs) seg.el.classList.toggle("lit", on && seg.d.path === path);
}

/* ---------------------------------------------------------------- side */
function select(item){
  S.sel = item;
  const meta = CATMETA[item.cat];
  const share = S.node.size ? (item.size / S.node.size) * 100 : 0;
  $("sidehd").innerHTML =
    '<div class="k">' + (item.kind === "file" ? "File" :
      item.kind === "rest" ? "Remainder" : "Folder") + "</div>" +
    '<div class="v mono">' + esc(item.path) + "</div>" +
    '<div class="m">' + fmtSize(item.size) + " &middot; " + share.toFixed(1) + "% of this view" +
      (item.files ? " &middot; " + fmtNum(item.files) + " files" : "") + "</div>" +
    (meta ? '<span class="tag ' + meta.confidence + '">' + esc(meta.label) + "</span>" +
      '<div class="hint">' + esc(meta.hint) + "</div>" : "");
  $("btn-reveal").disabled = item.kind === "rest";
}

function renderList(){
  const host = $("list");
  host.innerHTML = "";
  const kids = S.node.children || [];
  if(!kids.length){
    host.innerHTML = '<p class="empty">Nothing inside this folder is big enough to draw.</p>';
    return;
  }
  kids.forEach((kid, idx) => {
    const share = S.node.size ? kid.size / S.node.size : 0;
    /* Same rule the first ring uses, and the rows are in the same order, so a
       row and its slice always carry the same colour in either mode. */
    const hue = S.mode === "category" ? colorOf(kid.cat) : RAINBOW[idx % RAINBOW.length];
    const row = document.createElement("button");
    row.className = "item" + (kid.kind === "rest" ? " rest" : "");
    row.type = "button";
    row.dataset.path = kid.path;
    row.innerHTML =
      '<i style="background:' + hue + '"></i>' +
      '<span class="nm" title="' + esc(kid.name) + '">' + esc(kid.name) + "</span>" +
      '<span class="sz">' + fmtSize(kid.size) + "</span>" +
      '<span class="bar"><b style="width:' + (share * 100).toFixed(1) + "%;background:" +
        hue + '"></b></span>';
    row.onclick = () => {
      select(kid);
      if(kid.kind === "dir" && (kid.children || kid.more)) openNode(kid.path);
    };
    row.onmouseenter = () => litFromRow(kid.path, true);
    row.onmouseleave = () => litFromRow(kid.path, false);
    host.appendChild(row);
  });
}

function renderCrumbs(){
  const host = $("crumbs");
  host.innerHTML = "";
  S.node.crumbs.forEach((crumb, idx) => {
    if(idx){
      const sep = document.createElement("span");
      sep.className = "sep";
      sep.textContent = "\u203a";
      host.appendChild(sep);
    }
    const button = document.createElement("button");
    button.type = "button";
    button.textContent = crumb.name;
    button.title = crumb.path;
    button.onclick = () => openNode(crumb.path);
    host.appendChild(button);
  });
}

/* ------------------------------------------------------------ movement */
async function openNode(path){
  let view;
  try{ view = await api("/api/node" + (path ? "?path=" + encodeURIComponent(path) : "")); }
  catch(err){ toast("Could not open that folder", err.message, true); return; }
  S.node = view;
  unhover();
  drawMap();
  renderCrumbs();
  renderList();
  select(view);
}

async function enterMap(st){
  screen("map");
  try{ S.summary = await api("/api/summary"); }
  catch(err){ S.summary = null; }
  renderPills();
  renderStrip();
  renderLegend();
  await openNode("");
  const s = S.summary;
  if(s) toast("Scanned " + fmtSize(s.scan.size) + " in " + fmtDur(st.elapsed),
              fmtNum(s.scan.files) + " files across " + fmtNum(s.scan.dirs) + " folders");
}

/* ------------------------------------------------------------- actions */
async function revealSelected(){
  if(!S.sel) return;
  try{ await api("/api/reveal", {body: {path: S.sel.path}}); }
  catch(err){ toast("Could not reveal that", err.message, true); }
}

async function exportReport(){
  const button = $("btn-export");
  button.disabled = true;
  button.textContent = "Saving...";
  try{
    const out = await api("/api/export", {method: "POST"});
    toast("Saved " + out.files.join(", "), out.dir);
  }catch(err){
    toast("Could not save the report", err.message, true);
  }finally{
    button.disabled = false;
    button.textContent = "Save report";
  }
}

/* --------------------------------------------------------------- wiring */
$("btn-add").onclick = addCustom;
$("custom").addEventListener("keydown", ev => { if(ev.key === "Enter") addCustom(); });
$("btn-scan").onclick = startScan;
$("btn-cancel").onclick = async () => {
  S.cancelling = true;
  $("btn-cancel").disabled = true;
  $("btn-cancel").textContent = "Cancelling...";
  try{ await api("/api/cancel", {method: "POST"}); }catch(err){ /* the poll will report it */ }
};
$("btn-new").onclick = () => {
  // The totals belong to the scan being left behind, and "scanned 22 MB" next
  // to a drive picker reads as a claim about the drive you are about to pick.
  S.summary = null;
  renderPills();
  screen("setup");
  refreshGo();
};
$("btn-export").onclick = exportReport;
$("btn-reveal").onclick = revealSelected;
$("btn-mode").onclick = () => {
  S.mode = S.mode === "category" ? "rainbow" : "category";
  $("btn-mode").textContent = "Colour: " + (S.mode === "category" ? "category" : "by folder");
  renderLegend();
  if(S.node){ drawMap(); renderList(); }
};

let resizeTimer;
addEventListener("resize", () => {
  clearTimeout(resizeTimer);
  resizeTimer = setTimeout(() => { if(S.node) sizeHole(); }, 120);
});

addEventListener("keydown", ev => {
  if($("screen-map").hidden) return;
  if(ev.key === "Backspace" || ev.key === "Escape"){
    if(S.node && S.node.parent){ ev.preventDefault(); openNode(S.node.parent); }
  }
});

/* A scan may already be running or finished if the page was reloaded. */
(async function init(){
  await loadVolumes();
  let st;
  try{ st = await api("/api/status"); }catch(err){ return; }
  if(st.phase === "scanning" || st.phase === "analyzing"){ screen("scanning"); poll(); }
  else if(st.phase === "ready"){ await enterMap(st); }
})();
</script>
</body>
</html>
"""
