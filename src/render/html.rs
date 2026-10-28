//! Self-contained HTML rendering for elaborated freql process graphs.

use super::GraphDocument;

/// Render a graph document as a portable HTML file.
pub fn render_html(document: &GraphDocument) -> String {
    let json = serde_json::to_string(document).expect("GraphDocument must serialize");
    render_page(Some(&json), false)
}

/// Render the browser shell used by `freql serve`.
///
/// The shell listens to `/events` with `EventSource`. Each message may contain
/// either a serialized `GraphDocument` directly or `{ "document": ... }`.
pub fn live_html() -> String {
    render_page(None, true)
}

/// Render the live browser shell with an initial server update already embedded.
///
/// `initial_json` may be a `GraphDocument` or the server envelope
/// `{ "revision": ..., "graph": GraphDocument, "report": ..., "error": ... }`.
pub fn live_html_with_initial(initial_json: &str) -> String {
    render_page(Some(initial_json), true)
}

fn escape_embedded_json(json: &str) -> String {
    // JSON lives in a script element, whose contents are still terminated by
    // HTML parsing rules. Escaping `<` also handles every spelling of
    // `</script>` without changing the decoded JSON value.
    json.replace('&', "\\u0026")
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

fn render_page(initial_json: Option<&str>, live: bool) -> String {
    let initial = initial_json
        .map(escape_embedded_json)
        .unwrap_or_else(|| "null".to_owned());
    let live_flag = if live { "true" } else { "false" };

    format!(
        r####"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="color-scheme" content="light dark">
<title>freql graph</title>
<style>
:root {{
  color-scheme: light dark;
  --bg:#f5f6f8; --panel:#ffffff; --panel2:#eef1f5; --ink:#172033;
  --muted:#667085; --line:#d5dae3; --wire:#1778d4; --before:#7a5af8;
  --ok:#14804a; --partial:#a15c00; --bad:#c43246; --shadow:0 12px 36px #14213d18;
}}
@media (prefers-color-scheme:dark) {{ :root {{
  --bg:#0b0e14; --panel:#131823; --panel2:#1a2130; --ink:#edf2f7;
  --muted:#9ba7bb; --line:#2b3548; --wire:#54a8f3; --before:#ab92ff;
  --ok:#5bd49b; --partial:#efb95c; --bad:#ff7b8c; --shadow:0 16px 40px #0008;
}} }}
* {{ box-sizing:border-box }}
html,body {{ margin:0; min-height:100%; background:var(--bg); color:var(--ink); font:14px/1.45 ui-sans-serif,system-ui,-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif }}
body {{ height:100vh; overflow:hidden }}
header {{ height:58px; display:flex; align-items:center; gap:14px; padding:0 18px; background:var(--panel); border-bottom:1px solid var(--line) }}
.brand {{ font-weight:760; letter-spacing:-.02em; font-size:17px }}
.brand i {{ font-style:normal; color:var(--wire) }}
#parametersToggle {{ display:inline-flex; align-items:center; gap:6px; min-width:auto; white-space:nowrap }}
#parametersToggle::before {{ content:"‹"; font-size:17px; line-height:1; transition:transform .18s ease }}
#parametersToggle[aria-expanded="false"]::before {{ transform:rotate(180deg) }}
#entry {{ min-width:0; overflow:hidden; white-space:nowrap; text-overflow:ellipsis; color:var(--muted); font-family:ui-monospace,SFMono-Regular,Menlo,monospace }}
.spacer {{ flex:1 }}
.status {{ padding:4px 9px; border-radius:999px; font-weight:700; font-size:12px; background:var(--panel2) }}
.status.complete {{ color:var(--ok) }} .status.partial {{ color:var(--partial) }} .status.invalid,.status.error {{ color:var(--bad) }}
#liveState {{ color:var(--muted); font-size:12px }}
main {{ display:grid; grid-template-columns:330px minmax(0,1fr) 360px; height:calc(100vh - 58px); transition:grid-template-columns .18s ease }}
main.parameters-closed {{ grid-template-columns:0 minmax(0,1fr) 360px }}
.parameter-panel,.inspector-panel {{ overflow:auto; background:var(--panel); padding:18px 14px 32px; scrollbar-gutter:stable }}
.parameter-panel {{ min-width:0; border-right:1px solid var(--line); transition:opacity .12s ease,visibility .12s ease }}
.inspector-panel {{ border-left:1px solid var(--line) }}
main.parameters-closed .parameter-panel {{ visibility:hidden; opacity:0; padding-left:0; padding-right:0; border-right:0; pointer-events:none }}
.canvas-wrap {{ position:relative; min-width:0; overflow:hidden; background:radial-gradient(circle at 1px 1px,var(--line) 1px,transparent 1.2px); background-size:22px 22px }}
#graph {{ width:100%; height:100%; display:block; touch-action:none; user-select:none }}
.timeline {{ position:absolute; inset:0; overflow:auto; padding:76px 28px 36px; background:linear-gradient(135deg,color-mix(in srgb,var(--panel) 84%,transparent),color-mix(in srgb,var(--bg) 88%,transparent)) }}
.gantt {{ position:relative; min-width:900px; max-width:1500px }}
.gantt-axis,.gantt-row {{ display:grid; grid-template-columns:260px minmax(620px,1fr) }}
.gantt-axis {{ position:sticky; top:-76px; z-index:4; padding-top:8px; background:color-mix(in srgb,var(--bg) 94%,transparent) }}
.gantt-axis-label {{ display:flex; align-items:end; padding:0 12px 8px 0; color:var(--muted); font-size:11px; font-weight:750; letter-spacing:.06em; text-transform:uppercase }}
.gantt-scale {{ position:relative; height:34px; border-bottom:1px solid var(--line); color:var(--muted); font:10px ui-monospace,SFMono-Regular,Menlo,monospace }}
.gantt-tick {{ position:absolute; bottom:0; height:100%; border-left:1px solid var(--line) }}
.gantt-tick span {{ position:absolute; bottom:7px; left:5px; white-space:nowrap }}
.gantt-row {{ min-height:48px; border-bottom:1px solid color-mix(in srgb,var(--line) 68%,transparent); cursor:pointer }}
.gantt-row:hover {{ background:color-mix(in srgb,var(--wire) 5%,transparent) }}
.gantt-label {{ position:relative; min-width:0; padding:9px 12px 8px calc(12px + var(--depth) * 18px) }}
.gantt-label::before {{ content:""; position:absolute; left:calc(5px + var(--depth) * 18px); top:0; bottom:0; width:2px; background:color-mix(in srgb,var(--line) 75%,transparent) }}
.gantt-name {{ overflow:hidden; text-overflow:ellipsis; white-space:nowrap; font-size:13px; font-weight:750 }}
.gantt-meta {{ margin-top:2px; color:var(--muted); font:10px ui-monospace,SFMono-Regular,Menlo,monospace; overflow:hidden; text-overflow:ellipsis; white-space:nowrap }}
.gantt-track {{ position:relative; min-width:0; overflow:hidden; background-image:linear-gradient(to right,color-mix(in srgb,var(--line) 62%,transparent) 1px,transparent 1px); background-size:calc(100% / 5) 100% }}
.gantt-bar {{ position:absolute; z-index:1; top:13px; height:21px; min-width:4px; border-radius:5px; background:var(--wire); box-shadow:0 1px 2px #14213d24 }}
.gantt-bar.envelope {{ background:color-mix(in srgb,var(--wire) 8%,transparent); border:1px solid color-mix(in srgb,var(--wire) 26%,var(--line)); box-shadow:none }}
.gantt-bar.bounded {{ background:var(--before) }}
.gantt-bar.incomplete,.gantt-bar.open {{ border:1px dashed var(--partial); background:color-mix(in srgb,var(--partial) 16%,transparent); box-shadow:none }}
.gantt-density {{ position:absolute; z-index:2; min-width:1px; pointer-events:auto }}
.gantt-density.active {{ top:18px; height:11px; border-radius:3px; background:var(--wire) }}
.gantt-density.waiting {{ top:14px; height:19px; border-radius:3px; background:repeating-linear-gradient(135deg,color-mix(in srgb,var(--before) 78%,transparent) 0 3px,color-mix(in srgb,var(--before) 24%,transparent) 3px 6px) }}
.gantt-density.queued {{ top:9px; height:4px; border-radius:2px; background:var(--partial) }}
.gantt-uncertainty {{ position:absolute; top:13px; height:21px; min-width:4px; border-radius:0 5px 5px 0; background:repeating-linear-gradient(135deg,color-mix(in srgb,var(--before) 55%,transparent) 0 4px,transparent 4px 8px); border:1px solid color-mix(in srgb,var(--before) 70%,transparent); border-left:0 }}
.gantt-bar-label {{ position:absolute; z-index:3; top:36px; max-width:100%; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; color:var(--muted); font:10px ui-monospace,SFMono-Regular,Menlo,monospace }}
.gantt-open-label {{ color:var(--partial) }}
.gantt-legend {{ display:flex; flex-wrap:wrap; gap:9px 14px; margin:0 0 10px 260px; color:var(--muted); font-size:11px }}
.gantt-key {{ display:inline-flex; align-items:center; gap:5px }}
.gantt-key::before {{ content:""; width:14px; height:8px; border-radius:2px; background:var(--wire) }}
.gantt-key.envelope::before {{ background:color-mix(in srgb,var(--wire) 8%,transparent); border:1px solid color-mix(in srgb,var(--wire) 35%,var(--line)) }}
.gantt-key.waiting::before {{ background:repeating-linear-gradient(135deg,var(--before) 0 3px,transparent 3px 6px); border:1px solid color-mix(in srgb,var(--before) 70%,transparent) }}
.gantt-key.queued::before {{ height:4px; background:var(--partial) }}
.gantt-key.bounded::before {{ background:repeating-linear-gradient(135deg,var(--before) 0 3px,transparent 3px 6px); border:1px solid var(--before) }}
.gantt-key.open::before {{ background:transparent; border:1px dashed var(--partial) }}
.gantt-links {{ position:absolute; inset:0; z-index:2; overflow:visible; pointer-events:none }}
.gantt-link {{ fill:none; stroke:color-mix(in srgb,var(--muted) 82%,transparent); stroke-width:1.35; opacity:.9 }}
.gantt-link.repeated {{ stroke:var(--before); stroke-dasharray:4 3 }}
.toolbar {{ position:absolute; top:12px; left:12px; display:flex; gap:6px; padding:5px; border:1px solid var(--line); border-radius:10px; background:color-mix(in srgb,var(--panel) 92%,transparent); box-shadow:var(--shadow) }}
button {{ appearance:none; min-width:31px; height:30px; padding:0 9px; border:1px solid var(--line); border-radius:7px; background:var(--panel); color:var(--ink); cursor:pointer; font:inherit }}
button:hover {{ border-color:var(--wire) }} button:focus-visible {{ outline:2px solid var(--wire); outline-offset:2px }}
.legend {{ position:absolute; bottom:12px; left:12px; display:flex; gap:14px; padding:7px 10px; border:1px solid var(--line); border-radius:8px; color:var(--muted); background:color-mix(in srgb,var(--panel) 92%,transparent); font-size:12px }}
.key::before {{ content:""; width:20px; margin-right:6px; display:inline-block; vertical-align:middle; border-top:2px solid var(--wire) }}
.key.before::before {{ border-top:2px dashed var(--before) }}
h2 {{ margin:0 0 12px; font-size:15px }} h3 {{ margin:26px 0 9px; font-size:11px; color:var(--muted); font-weight:780; letter-spacing:.1em; text-transform:uppercase }}
#metrics > h3:first-child {{ margin-top:0 }}
.metric-list {{ display:grid; gap:7px }}
.metric-card {{ position:relative; min-width:0; padding:10px 11px 10px 13px; overflow:hidden; border:1px solid color-mix(in srgb,var(--line) 86%,transparent); border-radius:10px; background:linear-gradient(135deg,var(--panel2),color-mix(in srgb,var(--panel2) 72%,var(--panel))); box-shadow:0 1px 1px #14213d08 }}
.metric-card::before {{ content:""; position:absolute; inset:0 auto 0 0; width:3px; background:var(--wire); opacity:.65 }}
.metric-card.bounded::before {{ background:var(--before) }}
.metric-card.incomplete::before {{ background:var(--partial) }}
.metric-card-head {{ min-width:0 }}
.metric-label {{ min-width:0; font-weight:750; font-size:13px; letter-spacing:-.01em; overflow-wrap:anywhere }}
.metric-context {{ margin-top:2px; color:var(--muted); font:10px ui-monospace,SFMono-Regular,Menlo,monospace; overflow-wrap:anywhere }}
.metric-reading {{ display:flex; flex-wrap:wrap; align-items:baseline; gap:0 6px; margin-top:3px; font-variant-numeric:tabular-nums }}
.metric-number {{ font-size:17px; font-weight:690; letter-spacing:-.025em }}
.metric-unit {{ color:var(--muted); font-size:11px; font-weight:650 }}
.metric-status {{ margin-top:4px; color:var(--muted); font:10px ui-monospace,SFMono-Regular,Menlo,monospace }}
.metric-uncomputed {{ margin-top:7px; font-size:13px; font-weight:720 }}
.metric-dependency-label {{ margin-top:5px; color:var(--muted); font-size:11px }}
.metric-dependencies {{ margin:3px 0 0; padding-left:18px; color:var(--ink); font:10px/1.5 ui-monospace,SFMono-Regular,Menlo,monospace }}
.metric-dependencies li {{ overflow-wrap:anywhere }}
.metric-technical {{ margin-top:8px; padding-top:7px; border-top:1px solid color-mix(in srgb,var(--line) 72%,transparent) }}
.metric-technical summary {{ width:max-content; max-width:100%; color:var(--muted); cursor:pointer; font-size:10px; font-weight:690 }}
.metric-technical[open] summary {{ margin-bottom:8px; color:var(--ink) }}
.metric-technical .pairs {{ max-height:260px; padding-right:4px; overflow:auto; font-size:10px }}
.control-heading {{ display:flex; align-items:center; gap:8px; margin:0 0 9px }}
.control-heading h3 {{ margin:0; flex:1 }}
.control-heading button {{ height:27px; min-width:auto; padding:0 9px; border-radius:8px; color:var(--muted); font-size:11px }}
.control-heading button:hover {{ color:var(--ink); background:var(--panel2) }}
.control-search {{ width:100%; margin:0 0 8px; padding:7px 9px; border:1px solid var(--line); border-radius:8px; background:var(--panel2); color:var(--ink); font:12px ui-monospace,SFMono-Regular,Menlo,monospace }}
.control-search::placeholder {{ color:var(--muted) }}
.control {{ position:relative; margin:7px 0; padding:10px; overflow:hidden; border:1px solid var(--line); border-radius:10px; background:var(--panel2); box-shadow:0 1px 1px #14213d08 }}
.control.overridden {{ border-color:color-mix(in srgb,var(--wire) 72%,var(--line)); background:linear-gradient(135deg,color-mix(in srgb,var(--wire) 8%,var(--panel2)),var(--panel2)) }}
.control-title-row {{ display:flex; align-items:baseline; gap:7px; min-width:0 }}
.control-name {{ min-width:0; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; font-size:14px; font-weight:760; letter-spacing:-.015em }}
.control-badge {{ flex:none; padding:1px 5px; border-radius:999px; background:color-mix(in srgb,var(--wire) 14%,transparent); color:var(--wire); font-size:9px; font-weight:760; letter-spacing:.04em; text-transform:uppercase }}
.control-meta {{ display:block; min-width:0; margin:3px 0 8px; color:var(--muted); font:10px ui-monospace,SFMono-Regular,Menlo,monospace }}
.control-meta .control-path {{ display:block; min-width:0; margin-top:2px; overflow-wrap:anywhere }}
.control-meta .control-type {{ display:block; font-weight:700 }}
.control-row {{ display:flex; gap:6px }}
.control input {{ min-width:0; width:100%; padding:7px 8px; border:1px solid var(--line); border-radius:7px; background:var(--panel); color:var(--ink); font:12px ui-monospace,SFMono-Regular,Menlo,monospace; font-variant-numeric:tabular-nums }}
.control input:focus,.control-search:focus {{ outline:2px solid color-mix(in srgb,var(--wire) 55%,transparent); outline-offset:1px; border-color:var(--wire) }}
.control-row button {{ height:auto; min-width:auto; padding:0 8px; border-radius:7px; color:var(--muted); font-size:11px }}
.control-row button:hover {{ color:var(--ink); background:var(--panel) }}
.empty {{ color:var(--muted); padding:28px 12px; text-align:center }}
.item {{ margin:6px 0; padding:8px 9px; border-left:3px solid var(--line); border-radius:4px; background:var(--panel2); overflow-wrap:anywhere }}
.item.error {{ border-color:var(--bad) }} .item.warning {{ border-color:var(--partial) }}
.mono {{ font-family:ui-monospace,SFMono-Regular,Menlo,monospace; font-size:12px }}
.pairs {{ display:grid; grid-template-columns:max-content minmax(0,1fr); gap:5px 10px }} .pairs dt {{ color:var(--muted) }} .pairs dd {{ margin:0; overflow-wrap:anywhere }}
.node rect {{ fill:var(--panel); stroke:var(--line); stroke-width:1.5; filter:url(#shadow) }}
.node:hover rect,.node.selected rect {{ stroke:var(--wire); stroke-width:2.5 }}
.node {{ cursor:pointer }} .node .title {{ fill:var(--ink); font-size:14px; font-weight:700 }} .node .sub {{ fill:var(--muted); font-size:11px }} .node .kind {{ fill:var(--wire); font-size:10px; text-transform:uppercase; letter-spacing:.06em }}
.node .port-rule {{ stroke:var(--line); stroke-width:1 }}
.node .port-dot {{ fill:var(--panel); stroke:var(--wire); stroke-width:2 }}
.node .port-dot.export {{ fill:var(--wire) }}
.node .port-dot.uses {{ stroke:var(--before) }}
.node .port-name {{ fill:var(--ink); font-size:10px; font-family:ui-monospace,SFMono-Regular,Menlo,monospace }}
.node .metric-rule {{ stroke:var(--line); stroke-width:1 }}
.node .metric-text {{ fill:var(--ink); font-size:10px; font-family:ui-monospace,SFMono-Regular,Menlo,monospace }}
.node .metric-more {{ fill:var(--muted); font-size:10px }}
.metric-detail .metric-value {{ margin:2px 0 7px; color:var(--ink); font-weight:700 }}
.metric-detail .metric-uncomputed {{ margin-top:2px }}
.metric-detail .metric-status {{ margin:-3px 0 8px; color:var(--muted); font-size:12px }}
.metric-detail .pairs {{ font-size:12px }}
.toolbar button.active {{ border-color:var(--wire); color:var(--wire); background:var(--panel2) }}
.edge {{ fill:none; stroke-width:2 }} .edge.wire {{ stroke:var(--wire) }} .edge.before {{ stroke:var(--before); stroke-dasharray:7 5 }} .edge.other {{ stroke:var(--muted) }}
.edge-label {{ fill:var(--muted); font-size:10px; paint-order:stroke; stroke:var(--bg); stroke-width:4px; stroke-linejoin:round }}
.banner {{ position:absolute; inset:50% auto auto 50%; transform:translate(-50%,-50%); max-width:430px; padding:18px 22px; border:1px solid var(--line); border-radius:12px; background:var(--panel); color:var(--muted); text-align:center; box-shadow:var(--shadow) }}
.hidden {{ display:none!important }}
@media(max-width:1100px) {{
  main,main.parameters-closed {{ grid-template-columns:minmax(0,1fr) 340px }}
  .parameter-panel {{ position:fixed; z-index:20; top:58px; bottom:0; left:0; width:min(340px,calc(100vw - 48px)); border-right:1px solid var(--line); box-shadow:var(--shadow); transform:translateX(0); transition:transform .18s ease,visibility .18s ease }}
  main.parameters-closed .parameter-panel {{ visibility:hidden; opacity:1; padding:18px 14px 32px; border-right:1px solid var(--line); transform:translateX(-105%); pointer-events:none }}
}}
@media(max-width:760px) {{ main,main.parameters-closed {{ grid-template-columns:1fr; grid-template-rows:minmax(300px,60vh) 1fr; overflow:auto }} body {{ overflow:auto;height:auto }} .canvas-wrap {{ min-height:420px }} .inspector-panel {{ border-left:0;border-top:1px solid var(--line) }} .timeline {{ padding:68px 12px 24px }} .gantt {{ min-width:760px }} .gantt-axis,.gantt-row {{ grid-template-columns:210px minmax(550px,1fr) }} .gantt-legend {{ margin-left:210px }} #liveState {{ display:none }} }}
</style>
</head>
<body>
<header><div class="brand"><i>freql</i> graph</div><button id="parametersToggle" type="button" aria-controls="parameterPanel" aria-expanded="true" aria-label="Hide parameters">Parameters</button><div id="entry"></div><div class="spacer"></div><span id="liveState"></span><span id="status" class="status">waiting</span></header>
<main id="workspace">
  <aside id="parameterPanel" class="parameter-panel" aria-label="Exposed parameters"><div id="controls"></div></aside>
  <section class="canvas-wrap">
    <svg id="graph" role="img" aria-label="Process dependency graph"><defs>
      <marker id="arrow-wire" markerWidth="8" markerHeight="8" refX="7" refY="4" orient="auto"><path d="M0 0L8 4L0 8Z" fill="var(--wire)"/></marker>
      <marker id="arrow-before" markerWidth="8" markerHeight="8" refX="7" refY="4" orient="auto"><path d="M0 0L8 4L0 8Z" fill="var(--before)"/></marker>
      <marker id="arrow-other" markerWidth="8" markerHeight="8" refX="7" refY="4" orient="auto"><path d="M0 0L8 4L0 8Z" fill="var(--muted)"/></marker>
      <filter id="shadow" x="-20%" y="-30%" width="140%" height="160%"><feDropShadow dx="0" dy="3" stdDeviation="4" flood-opacity=".12"/></filter>
    </defs><g id="viewport"><g id="edges"></g><g id="nodes"></g></g></svg>
    <div id="timeline" class="timeline hidden" aria-live="polite"></div>
    <div class="toolbar" aria-label="Graph controls"><button id="graphView" class="active" title="Show process graph" aria-pressed="true">Graph</button><button id="timelineView" title="Show conservative Gantt schedule" aria-pressed="false">Gantt</button><button id="zoomOut" title="Zoom out">−</button><button id="zoomIn" title="Zoom in">+</button><button id="fit" title="Fit graph">Fit</button><button id="ports" class="active" title="Show or hide process ports" aria-pressed="true">Ports: on</button></div>
    <div class="legend"><span class="key">physical transfer</span><span class="key before">causal order</span></div>
    <div id="banner" class="banner hidden"></div>
  </section>
  <aside class="inspector-panel" aria-label="Analysis and run details"><div id="metrics"></div><div id="details"></div><div id="report"></div></aside>
</main>
<script id="freql-document" type="application/json">{initial}</script>
<script>
(() => {{
  'use strict';
  const LIVE = {live_flag};
  const NS = 'http://www.w3.org/2000/svg';
  const svg = document.getElementById('graph'), viewport = document.getElementById('viewport');
  const nodesLayer = document.getElementById('nodes'), edgesLayer = document.getElementById('edges');
  const timeline = document.getElementById('timeline');
  const workspace = document.getElementById('workspace'), parameterPanel = document.getElementById('parameterPanel'), parametersToggle = document.getElementById('parametersToggle');
  let current = null, selected = null, portsVisible = true, activeView = 'graph', nodeMetrics = new Map(), temporalMetrics = new Map(), streamHandoffs = [], streamCardinalities = [], resourceSchedules = [], provisionedResources = [], layoutBounds = {{x:0,y:0,w:100,h:100}};
  const controlTimers = new Map();
  let controlFilter = '';
  let storedParametersOpen = null;
  try {{ storedParametersOpen=sessionStorage.getItem('freql.parameters.open') }} catch(_error) {{}}
  let parametersOpen = LIVE && (storedParametersOpen==null?!matchMedia('(max-width:1100px)').matches:storedParametersOpen==='true');
  let view = {{x:0,y:0,w:100,h:100}}, dragging = null;
  const arr = value => Array.isArray(value) ? value : [];
  const str = value => value == null ? '' : typeof value === 'string' ? value : JSON.stringify(value);
  const setParametersOpen = (open,persist=true) => {{
    parametersOpen=Boolean(open)&&LIVE;workspace.classList.toggle('parameters-closed',!parametersOpen);parameterPanel.setAttribute('aria-hidden',String(!parametersOpen));parameterPanel.toggleAttribute('inert',!parametersOpen);parametersToggle.setAttribute('aria-expanded',String(parametersOpen));parametersToggle.setAttribute('aria-label',parametersOpen?'Hide parameters':'Show parameters');parametersToggle.title=parametersOpen?'Hide exposed parameters':'Show exposed parameters';
    if(persist)try {{ sessionStorage.setItem('freql.parameters.open',String(parametersOpen)) }} catch(_error) {{}}
  }};
  const scaledInteger = (digits,exponent) => {{
    const negative=digits.startsWith('-'),body=negative?digits.slice(1):digits,split=body.length-exponent;let value=split>0?body.slice(0,split)+'.'+body.slice(split):'0.'+'0'.repeat(-split)+body;
    if(value.includes('.'))value=value.replace(/0+$/,'').replace(/\.$/,'');return (negative?'-':'')+value;
  }};
  // Reports retain exact values. Only monitored-value presentation truncates
  // toward zero to at most three decimal places.
  const truncateMonitoredText = value => {{
    const text=str(value),decimal=/^(-?)(\d+)(?:\.(\d+))?(\s+.*)?$/.exec(text);
    if(decimal){{const fraction=decimal[3]||'';if(fraction.length<=3)return text;return decimal[1]+decimal[2]+'.'+fraction.slice(0,3)+(decimal[4]||'')}}
    const rational=/^(-?\d+)\/(\d+)(\s+.*)?$/.exec(text);if(!rational)return text;
    try{{const numerator=BigInt(rational[1]),denominator=BigInt(rational[2]);if(denominator===0n)return text;const negative=numerator<0n,magnitude=negative?-numerator:numerator,whole=magnitude/denominator,scaled=(magnitude%denominator)*1000n/denominator;const fraction=scaled.toString().padStart(3,'0').replace(/0+$/,'');const sign=negative&&(whole!==0n||scaled!==0n)?'-':'';return sign+whole.toString()+(fraction?'.'+fraction:'')+(rational[3]||'')}}catch(_error){{return text}}
  }};
  const quantityParts = (value,preserve=false) => {{const match=/^(\S+)(?:\s+(.+))?$/.exec(str(value));return match?{{number:preserve?match[1]:truncateMonitoredText(match[1]),unit:match[2]||''}}:{{number:truncateMonitoredText(value),unit:''}}}};
  const isConcreteMetricValue = value => /^(?:true|false|-?(?:\d+(?:\.\d+)?|\d+\/\d+)(?:\s+.+)?)$/.test(str(value).trim());
  const metricBoundValue = metric => {{const bounds=metric&&metric.bounds;if(!bounds)return'';const lower=quantityParts(bounds.lower),upper=quantityParts(bounds.upper);return lower.unit===upper.unit?`${{lower.number}}–${{upper.number}}${{lower.unit?' '+lower.unit:''}}`:`${{lower.number}}${{lower.unit?' '+lower.unit:''}}–${{upper.number}}${{upper.unit?' '+upper.unit:''}}`}};
  const temporalForMetric = metric => temporalMetrics.get(str(metric&&metric.path));
  const temporalRangeValue = temporal => {{const known=temporal&&temporal.known_work;if(!known)return'';const lower=quantityParts(known.lower),upper=quantityParts(known.upper);return lower.unit===upper.unit?`${{lower.number}}–${{upper.number}}${{lower.unit?' '+lower.unit:''}}`:`${{lower.number}}${{lower.unit?' '+lower.unit:''}}–${{upper.number}}${{upper.unit?' '+upper.unit:''}}`}};
  const metricPresentation = metric => {{
    const temporal=temporalForMetric(metric),bounded=metric&&metric.bounds&&isConcreteMetricValue(metric.bounds.lower)&&isConcreteMetricValue(metric.bounds.upper);if(metric&&metric.approximate_value!=null&&!bounded&&(!temporal||str(temporal.status)!=='incomplete'))return {{value:metric.approximate_value,decimal:true,knownWork:false,incomplete:false,symbolic:false}};if(!temporal){{const value=bounded?metricBoundValue(metric):truncateMonitoredText(metric&&metric.value);return {{value,reason:metric&&metric.bounds&&metric.bounds.reason,knownWork:false,incomplete:false,symbolic:!bounded&&!isConcreteMetricValue(metric&&metric.value)}}}}
    const incomplete=str(temporal.status)==='incomplete',knownWork=temporalRangeValue(temporal)||truncateMonitoredText(metric&&metric.value);
    const concreteKnown=temporal&&temporal.known_work&&isConcreteMetricValue(temporal.known_work.lower)&&isConcreteMetricValue(temporal.known_work.upper);
    if(incomplete){{const count=arr(temporal.obligations).length;return {{value:knownWork,reason:`known work only · successful completion has ${{count}} open duration obligation${{count===1?'':'s'}}`,knownWork:true,incomplete:true,symbolic:!concreteKnown}}}}
    return {{value:knownWork,reason:temporal.reason||null,knownWork:false,incomplete:false,symbolic:!concreteKnown&&!isConcreteMetricValue(metric&&metric.value)}};
  }};
  const monitoredValue = metric => metricPresentation(metric).value;
  const metricScales = metric => {{
    if(metric&&metric.approximate_value!=null)return[];const match=/^(-?\d+)(?:\s+(.+))?$/.exec(str(metric&&metric.value));if(!match)return[];const digits=match[1],unit=match[2]||'';
    return [[24,'Y'],[21,'Z'],[18,'E'],[15,'P'],[12,'T'],[9,'G']].filter(([exponent])=>digits.replace('-','').length>exponent).map(([exponent,prefix])=>truncateMonitoredText(scaledInteger(digits,exponent)+' '+prefix+unit));
  }};
  const svgEl = (name, attrs={{}}) => {{ const el=document.createElementNS(NS,name); for(const [k,v] of Object.entries(attrs)) el.setAttribute(k,v); return el; }};
  const textEl = (x,y,value,klass) => {{ const el=svgEl('text',{{x,y,class:klass}}); el.textContent=str(value); return el; }};
  const itemText = item => typeof item === 'string' ? item : item.message || item.predicate || (item.makespan != null ? `${{item.path}}: ${{item.makespan}} · ${{item.jobs}} jobs × ${{item.repetitions}} repeats` : item.value != null ? `${{item.path}}: ${{item.value}}` : item.expression || item.path) || JSON.stringify(item);
  const edgeKind = edge => {{ const k=str(edge.kind).toLowerCase(); return k.includes('before') || k.includes('causal') || k.includes('order') ? 'before' : k.includes('wire') || k.includes('transfer') || k.includes('physical') ? 'wire' : 'other'; }};
  const portRole = member => str(member&&member.role).toLowerCase();
  const nodePathCandidates = node => {{
    const candidates=new Set();
    for(const value of [node&&node.id,node&&node.path]){{
      const path=str(value);if(!path)continue;candidates.add(path);
      const separator=path.lastIndexOf('::');if(separator>=0)candidates.add(path.slice(separator+2));
    }}
    return [...candidates].sort((a,b)=>b.length-a.length);
  }};
  const associateMetrics = (nodes,metrics) => {{
    const associated=new Map(nodes.map(node=>[str(node.id),[]]));
    for(const metric of metrics){{
      const path=str(metric&&metric.path);let best=null;
      for(const node of nodes){{
        const kind=str(node&&node.kind).toLowerCase();if(kind!=='run'&&kind!=='process')continue;
        for(const prefix of nodePathCandidates(node)){{
          if((path===prefix||path.startsWith(prefix+'.'))&&(!best||prefix.length>best.prefix.length))best={{node,prefix}};
        }}
      }}
      if(best)associated.get(str(best.node.id)).push({{metric,prefix:best.prefix}});
    }}
    for(const entries of associated.values())entries.sort((a,b)=>str(a.metric.path).localeCompare(str(b.metric.path)));
    return associated;
  }};
  const metricsForNode = node => nodeMetrics.get(str(node&&node.id))||[];
  const nodePorts = node => {{
    if(!portsVisible) return {{inputs:[],outputs:[]}};
    // Boundary input/export nodes already represent a single external port.
    // Adding a second port glyph to them would reverse the visual anchor for
    // parent-input sources and parent-export targets.
    const kind=str(node&&node.kind).toLowerCase();
    if(kind==='input'||kind==='export') return {{inputs:[],outputs:[]}};
    const members=arr(node&&node.members);
    return {{
      inputs:members.filter(member=>['gets','uses'].includes(portRole(member))),
      outputs:members.filter(member=>portRole(member)==='export')
    }};
  }};
  const shortText = (value,limit) => {{ const text=str(value); return text.length>limit?text.slice(0,Math.max(1,limit-1))+'…':text; }};
  const shortPort = value => shortText(value,20);
  const metricName = entry => {{const path=str(entry.metric&&entry.metric.path),prefix=str(entry.prefix);return path.startsWith(prefix+'.')?path.slice(prefix.length+1):path.split('.').pop()||path}};
  const metricSummary = entry => {{const presentation=metricPresentation(entry.metric),scales=metricScales(entry.metric),value=presentation.symbolic?'not yet computable':scales[0]||presentation.value;return shortText(`${{metricName(entry)}}: ${{value}}${{presentation.knownWork&&!presentation.symbolic?' (known)':''}}`,34)}};
  const terminalMetricName = metric => str(metric&&metric.path).split('.').filter(Boolean).pop()||str(metric&&metric.path);
  const metricInstancePath = metric => {{const parts=str(metric&&metric.path).split('.').filter(Boolean);parts.pop();return parts.join('.')}};
  const isAncestorMetric = (ancestor,descendant) => {{const parent=metricInstancePath(ancestor),child=metricInstancePath(descendant);return parent!==child&&child.startsWith(parent+'.')}};
  const compilationMetrics = metrics => {{
    // A composite's total shadows the same metric on its descendants (for
    // example Factory.cost over Factory.batch.cost). Sibling fleet metrics are
    // all retained, so upload and download rates stay separately visible.
    return metrics.filter((metric,index)=>!metrics.some((other,otherIndex)=>
      otherIndex!==index&&terminalMetricName(other)===terminalMetricName(metric)&&isAncestorMetric(other,metric)
    ));
  }};
  const humanize = value => str(value).replace(/_/g,' ').replace(/\b\w/g,character=>character.toUpperCase());
  const metricPathContext = metric => {{
    const parts=str(metric&&metric.path).split('.').filter(Boolean);parts.pop();
    if(parts.length>3)return `${{parts[0]}} › … › ${{parts.at(-1)}}`;
    return parts.join(' › ');
  }};
  const directTemporalForNode = node => {{
    for(const candidate of nodePathCandidates(node)){{
      const prefix=candidate+'.',matches=[...temporalMetrics.values()].filter(metric=>{{const path=str(metric&&metric.path);return path.startsWith(prefix)&&!path.slice(prefix.length).includes('.')}});
      if(matches.length)return matches.sort((left,right)=>str(left.path).localeCompare(str(right.path)));
    }}
    return [];
  }};
  const temporalMetricName = metric => str(metric&&metric.path).split('.').pop()||'time';
  const streamHandoffForEdge = (edge,byId) => {{
    if(!str(edge&&edge.ty).startsWith('Stream['))return null;
    const producer=byId.get(str(edge&&edge.source)),stream=str(edge&&edge.source_port);
    if(!producer||!stream)return null;
    for(const path of nodePathCandidates(producer)){{
      const handoff=streamHandoffs.find(candidate=>str(candidate&&candidate.path)===path&&str(candidate&&candidate.stream)===stream);
      if(handoff)return handoff;
    }}
    return null;
  }};
  const streamCardinalityForEdge = (edge,byId) => {{
    if(!str(edge&&edge.ty).startsWith('Stream['))return null;
    const producer=byId.get(str(edge&&edge.source)),stream=str(edge&&edge.source_port);
    if(!producer||!stream)return null;
    for(const path of nodePathCandidates(producer)){{
      const cardinality=streamCardinalities.find(candidate=>str(candidate&&candidate.path)===path&&str(candidate&&candidate.stream)===stream);
      if(cardinality)return cardinality;
    }}
    return null;
  }};
  const exactNumber = value => {{
    const text=str(value).trim();if(!text)return null;try{{if(text.includes('/')){{const [numerator,denominator]=text.split('/');const top=Number(BigInt(numerator)),bottom=Number(BigInt(denominator));return bottom===0?null:top/bottom}}const parsed=Number(text);return Number.isFinite(parsed)?parsed:null}}catch(_error){{return null}}
  }};
  const temporalSeconds = (metric,bound) => exactNumber(metric&&metric.known_work&&metric.known_work[bound+'_seconds']);
  const formatElapsed = seconds => {{
    if(seconds==null||!Number.isFinite(seconds))return 'unknown';const day=86400,year=365*day;
    if(seconds>=year)return `${{truncateMonitoredText(String(seconds/year))}} yr`;
    if(seconds>=day)return `${{truncateMonitoredText(String(seconds/day))}} d`;
    if(seconds>=3600)return `${{truncateMonitoredText(String(seconds/3600))}} h`;
    return `${{truncateMonitoredText(String(seconds))}} s`;
  }};
  const ganttAxis = (extent,labelText='Run') => {{
    const axis=document.createElement('div');axis.className='gantt-axis';const axisLabel=document.createElement('div');axisLabel.className='gantt-axis-label';axisLabel.textContent=labelText;const scale=document.createElement('div');scale.className='gantt-scale';for(let index=0;index<=5;index++){{const tick=document.createElement('div');tick.className='gantt-tick';tick.style.left=`${{index*20}}%`;const label=document.createElement('span');label.textContent=formatElapsed(extent*index/5);tick.append(label);scale.append(tick)}}axis.append(axisLabel,scale);return axis;
  }};
  const renderActivityDensity = (track,task,extent) => {{
    const buckets=arr(task.scheduled&&task.scheduled.stage.activity).map(bucket=>({{
      start:exactNumber(bucket&&bucket.start_seconds),end:exactNumber(bucket&&bucket.end_seconds),
      active:exactNumber(bucket&&bucket.average_active_operations)||0,
      waiting:exactNumber(bucket&&bucket.average_waiting_operations)||0,
      queued:exactNumber(bucket&&bucket.average_queued_operations)||0
    }})).filter(bucket=>bucket.start!=null&&bucket.end!=null&&bucket.end>bucket.start);
    if(!buckets.length)return;
    const shift=task.scheduledStart==null?0:task.start-task.scheduledStart;
    const maxima={{active:Math.max(...buckets.map(bucket=>bucket.active),0),waiting:Math.max(...buckets.map(bucket=>bucket.waiting),0),queued:Math.max(...buckets.map(bucket=>bucket.queued),0)}};
    for(const state of ['waiting','active','queued'])for(const bucket of buckets){{
      const value=bucket[state];if(!(value>0))continue;
      const segment=document.createElement('div');segment.className=`gantt-density ${{state}}`;
      segment.style.left=`${{Math.max(0,Math.min(100,(bucket.start+shift)/extent*100))}}%`;
      segment.style.width=`${{Math.max(.08,Math.min(100,(bucket.end-bucket.start)/extent*100))}}%`;
      segment.style.opacity=String(.2+.8*Math.sqrt(value/maxima[state]));
      segment.title=`${{formatElapsed(bucket.start+shift)}}–${{formatElapsed(bucket.end+shift)}} · average ${{state}} operations: ${{truncateMonitoredText(String(value))}}`;
      track.append(segment);
    }}
  }};
  const drawGanttLinks = (gantt,rowsById,links,extent) => {{
    gantt.querySelector('.gantt-links')?.remove();if(!links.length||extent<=0)return;
    const bounds=gantt.getBoundingClientRect(),overlay=svgEl('svg',{{class:'gantt-links',width:String(gantt.scrollWidth),height:String(gantt.scrollHeight),viewBox:`0 0 ${{gantt.scrollWidth}} ${{gantt.scrollHeight}}`}}),defs=svgEl('defs'),marker=svgEl('marker',{{id:'gantt-enable-arrow',markerWidth:'7',markerHeight:'7',refX:'6',refY:'3.5',orient:'auto',markerUnits:'strokeWidth'}});marker.append(svgEl('path',{{d:'M0 0L7 3.5L0 7Z',fill:'context-stroke'}}));defs.append(marker);overlay.append(defs);
    const seen=new Set();for(const link of links){{const key=`${{link.source}}>${{link.target}}>${{link.repeated?'r':'w'}}`;if(seen.has(key))continue;seen.add(key);const sourceRow=rowsById.get(str(link.source)),targetRow=rowsById.get(str(link.target));if(!sourceRow||!targetRow)continue;const sourceBar=sourceRow.querySelector('.gantt-bar'),targetBar=targetRow.querySelector('.gantt-bar'),sourceTrack=sourceRow.querySelector('.gantt-track');if(!sourceBar||!targetBar||!sourceTrack)continue;const sourceRect=sourceBar.getBoundingClientRect(),targetRect=targetBar.getBoundingClientRect(),trackRect=sourceTrack.getBoundingClientRect(),sourceX=link.sourceTime==null?sourceRect.right-bounds.left:trackRect.left-bounds.left+Math.max(0,Math.min(1,link.sourceTime/extent))*trackRect.width,targetX=targetRect.left-bounds.left-3,sourceY=sourceRect.top-bounds.top+sourceRect.height/2,targetY=targetRect.top-bounds.top+targetRect.height/2,bend=targetX>=sourceX?sourceX+Math.max(10,(targetX-sourceX)/2):Math.max(sourceX,targetX)+18,path=svgEl('path',{{d:`M ${{sourceX}} ${{sourceY}} H ${{bend}} V ${{targetY}} H ${{targetX}}`,class:`gantt-link${{link.repeated?' repeated':''}}`,'marker-end':'url(#gantt-enable-arrow)'}}),title=svgEl('title');title.textContent=link.label||(link.repeated?'per-operation enablement':'causal enablement');path.append(title);overlay.append(path)}}gantt.append(overlay);
  }};
  function renderTimeline() {{
    timeline.replaceChildren();
    let attachedMetrics,stageMin,stageMax,stageDuration,baseMeta;
    const title=document.createElement('h2');title.textContent='Gantt';timeline.append(title);
    const allNodes=arr(current&&current.nodes).filter(node=>{{const kind=str(node&&node.kind).toLowerCase();return kind==='process'||kind==='run'}});
    if(!allNodes.length){{const empty=document.createElement('div');empty.className='empty';empty.textContent='No process runs are available for scheduling.';timeline.append(empty);return}}
    const byId=new Map(allNodes.map(node=>[str(node.id),node])),children=new Map(allNodes.map(node=>[str(node.id),[]]));allNodes.forEach(node=>{{const parent=str(node.parent);if(children.has(parent))children.get(parent).push(node)}});for(const values of children.values())values.sort((left,right)=>str(left.id).localeCompare(str(right.id)));
    const scheduledStages=new Map();resourceSchedules.forEach(schedule=>arr(schedule&&schedule.stages).forEach(stage=>scheduledStages.set(str(stage&&stage.path),{{schedule,stage}})));
    const scheduleForNode=node=>{{for(const path of nodePathCandidates(node)){{const scheduled=scheduledStages.get(path);if(scheduled)return scheduled}}return null}};
    const containingScheduleOffset=node=>{{let best=null;for(const candidate of nodePathCandidates(node))for(const schedule of resourceSchedules){{const processPath=str(schedule&&schedule.path).split('.schedule[')[0];if(candidate!==processPath&&!candidate.startsWith(processPath+'.'))continue;if(!best||processPath.length>best.path.length)best={{path:processPath,offset:exactNumber(schedule.start_offset_seconds)||0}}}}return best?best.offset:0}};
    const tasks=allNodes.filter(node=>!node.composite).map(node=>{{
      const temporal=directTemporalForNode(node).find(metric=>temporalMetricName(metric)==='duration')||null,incomplete=str(temporal&&temporal.status)==='incomplete',scheduled=scheduleForNode(node),scheduledReady=exactNumber(scheduled&&scheduled.stage.earliest_ready_seconds),scheduledStart=exactNumber(scheduled&&scheduled.stage.earliest_start_seconds),scheduledFinish=exactNumber(scheduled&&scheduled.stage.latest_finish_seconds);
      const containerOffset=containingScheduleOffset(node);return {{id:str(node.id),node,temporal,scheduled,scheduleKey:str(scheduled&&scheduled.schedule.path),scheduledReady,scheduledStart,scheduledFinish,containerOffset,lower:incomplete?null:temporalSeconds(temporal,'lower'),upper:incomplete?null:temporalSeconds(temporal,'upper'),start:scheduledStart==null?containerOffset:scheduledStart,finish:scheduledFinish,streamClose:0,blocked:false,streamNotes:[]}}
    }}),byTask=new Map(tasks.map(task=>[task.id,task])),predecessors=new Map(tasks.map(task=>[task.id,[]]));
    arr(current&&current.edges).forEach(edge=>{{const source=str(edge.source),target=str(edge.target),sourceTask=byTask.get(source),targetTask=byTask.get(target);if(!sourceTask||!targetTask)return;const stream=str(edge.ty).startsWith('Stream['),ordered=edgeKind(edge)==='before';if(ordered||(!stream&&edgeKind(edge)==='wire')){{predecessors.get(target).push({{source,kind:'completion'}});return}}if(stream){{const handoff=streamHandoffForEdge(edge,byId),cardinality=streamCardinalityForEdge(edge,byId),delay=exactNumber(handoff&&handoff.first_item_after_seconds),streamLabel=[str(edge.ty),cardinality&&`${{str(cardinality.cardinality)}} items`].filter(Boolean).join(' · ');if(delay!=null){{predecessors.get(target).push({{source,kind:'handoff',delay,label:`${{streamLabel}} · first after ${{str(handoff.first_item_after)}}`}});targetTask.streamNotes.push(`${{streamLabel}} · first after ${{str(handoff.first_item_after)}}`)}}else targetTask.streamNotes.push(`${{streamLabel||str(edge.source_port)||'stream'}} · first-item timing unspecified`);predecessors.get(target).push({{source,kind:'stream-close',label:`${{streamLabel}} · close`}})}}}});
    // A scheduled borrower cannot start before the process that constructed
    // its original pool. Resource paths already resolve aliases/re-exports to
    // that stable origin, so this remains generic across domains.
    const taskForInstance=path=>tasks.find(task=>nodePathCandidates(task.node).includes(path));
    for(const task of tasks){{if(!task.scheduled)continue;for(const resourcePath of arr(task.scheduled.stage.resources)){{const parts=str(resourcePath).split('.');parts.pop();const origin=taskForInstance(parts.join('.'));if(origin&&!predecessors.get(task.id).some(value=>value.source===origin.id&&value.kind==='completion'))predecessors.get(task.id).push({{source:origin.id,kind:'completion',resource:true}})}}}}
    const scheduleOffsets=new Map(tasks.filter(task=>task.scheduleKey&&task.scheduledStart!=null&&task.scheduledFinish!=null).map(task=>[task.scheduleKey,0]));
    for(let pass=0;pass<tasks.length;pass++){{
      const scheduleBlocked=new Map([...scheduleOffsets.keys()].map(key=>[key,false]));
      for(const task of tasks){{
        if(!task.scheduleKey||task.scheduledStart==null||task.scheduledFinish==null)continue;
        let offset=scheduleOffsets.get(task.scheduleKey)||0;
        for(const predecessor of predecessors.get(task.id)){{
          const prior=byTask.get(predecessor.source);
          // The operational scheduler has already applied dependencies among
          // stages in the same repeated-job schedule. Reapplying a single
          // aggregate process edge here would serialize every prepare before
          // every inspection instead of displaying their pipelined overlap.
          if(prior&&prior.scheduleKey===task.scheduleKey)continue;
          if(!prior||prior.blocked||((predecessor.kind==='completion'||predecessor.kind==='stream-close')&&prior.finish==null)){{scheduleBlocked.set(task.scheduleKey,true);continue}}
          const required=predecessor.kind==='handoff'?prior.start+predecessor.delay:prior.finish;
          if(required!=null)offset=Math.max(offset,required-task.scheduledStart);
        }}
        scheduleOffsets.set(task.scheduleKey,Math.max(0,offset));
      }}
      for(const task of tasks){{
        if(!task.scheduleKey||task.scheduledStart==null||task.scheduledFinish==null)continue;
        const offset=scheduleOffsets.get(task.scheduleKey)||0;task.start=task.scheduledStart+offset;task.finish=task.scheduledFinish+offset;task.streamClose=0;task.blocked=scheduleBlocked.get(task.scheduleKey)===true;
      }}
      for(const task of tasks){{
        if(task.scheduleKey&&task.scheduledStart!=null&&task.scheduledFinish!=null)continue;
        let start=task.containerOffset||0,streamClose=0,blocked=false;
        for(const predecessor of predecessors.get(task.id)){{const prior=byTask.get(predecessor.source);if(!prior||prior.blocked||((predecessor.kind==='completion'||predecessor.kind==='stream-close')&&prior.finish==null)){{blocked=true;continue}}if(predecessor.kind==='handoff')start=Math.max(start,prior.start+predecessor.delay);else if(predecessor.kind==='stream-close')streamClose=Math.max(streamClose,prior.finish);else start=Math.max(start,prior.finish)}}
        task.start=start;task.streamClose=streamClose;task.blocked=blocked;task.finish=blocked||task.upper==null?null:Math.max(start+task.upper,streamClose);
      }}
    }}
    const extent=Math.max(1,...tasks.filter(task=>task.finish!=null&&!task.blocked).map(task=>task.finish));
    const legend=document.createElement('div');legend.className='gantt-legend';for(const [label,klass] of [['stage lifetime','envelope'],['active operations',''],['waiting / draining','waiting'],['queued / backlog','queued'],['bounded range','bounded'],['open duration','open']]){{const key=document.createElement('span');key.className='gantt-key '+klass;key.textContent=label;legend.append(key)}}timeline.append(legend);
    const gantt=document.createElement('div');gantt.className='gantt';gantt.append(ganttAxis(extent));
    const roots=arr(current&&current.roots).map(id=>byId.get(str(id))).filter(Boolean);const visibleRoots=roots.length?roots:allNodes.filter(node=>!byId.has(str(node.parent)));const orderedTasks=[];const seen=new Set();const visit=(node,depth)=>{{const id=str(node.id),task=byTask.get(id);if(task&&!seen.has(id)){{task.depth=depth;seen.add(id);orderedTasks.push(task)}}arr(children.get(id)).forEach(child=>visit(child,depth+1))}};visibleRoots.sort((left,right)=>str(left.id).localeCompare(str(right.id))).forEach(root=>visit(root,0));tasks.filter(task=>!seen.has(task.id)).sort((left,right)=>left.id.localeCompare(right.id)).forEach(task=>{{task.depth=0;orderedTasks.push(task)}});
    const rowsById=new Map();
    orderedTasks.forEach(task=>{{
      const row=document.createElement('div');row.className='gantt-row';row.style.setProperty('--depth',String(task.depth||0));
      const label=document.createElement('div');label.className='gantt-label';
      const name=document.createElement('div');name.className='gantt-name';name.textContent=str(task.node.label||task.id.split('.').pop());
      const meta=document.createElement('div');meta.className='gantt-meta';
      attachedMetrics=metricsForNode(task.node).slice(0,2).map(metricSummary);
      stageMin=exactNumber(task.scheduled&&task.scheduled.stage.minimum_duration_seconds);
      stageMax=exactNumber(task.scheduled&&task.scheduled.stage.maximum_duration_seconds);
      stageDuration=stageMin!=null&&stageMax!=null&&Math.abs(stageMax-stageMin)>.001?`${{formatElapsed(stageMin)}}–${{formatElapsed(stageMax)}}`:str(task.scheduled&&task.scheduled.stage.duration);
      baseMeta=task.scheduled?`${{task.scheduled.stage.operation_count}} operations · ${{stageDuration}} each`:task.temporal?`${{temporalRangeValue(task.temporal)}} · ${{str(task.temporal.status)}}`:'duration open';
      meta.textContent=[baseMeta,...attachedMetrics].join(' · ');label.append(name,meta);
      const track=document.createElement('div');track.className='gantt-track';
      const scheduledWindow=task.scheduled&&task.scheduledStart!=null&&task.scheduledFinish!=null;
      const left=task.blocked?0:Math.min(100,task.start/extent*100);
      if((!scheduledWindow&&task.upper==null)||task.blocked){{
        const open=document.createElement('div');open.className='gantt-bar open';open.style.left=`${{left}}%`;open.style.width='13%';
        const note=document.createElement('div');note.className='gantt-bar-label gantt-open-label';note.style.left=`${{left}}%`;note.textContent=task.blocked?'start blocked by open predecessor':'duration open';track.append(open,note);
      }}else if(scheduledWindow){{
        const shift=task.start-task.scheduledStart;
        const ready=(task.scheduledReady==null?task.scheduledStart:task.scheduledReady)+shift;
        const envelopeStart=Math.min(ready,task.start),envelopeLeft=Math.max(0,Math.min(100,envelopeStart/extent*100)),envelopeWidth=Math.max(.2,(task.finish-envelopeStart)/extent*100);
        const envelope=document.createElement('div');envelope.className='gantt-bar envelope';envelope.style.left=`${{envelopeLeft}}%`;envelope.style.width=`${{envelopeWidth}}%`;
        const note=document.createElement('div');note.className='gantt-bar-label';note.style.left=`${{envelopeLeft}}%`;
        note.textContent=ready+0.001<task.start?`ready ${{formatElapsed(ready)}} · starts ${{formatElapsed(task.start)}} · ${{task.scheduled.stage.operation_count}} operations through ${{formatElapsed(task.finish)}}`:`starts ${{formatElapsed(task.start)}} · ${{task.scheduled.stage.operation_count}} operations through ${{formatElapsed(task.finish)}}`;
        track.append(envelope);renderActivityDensity(track,task,extent);track.append(note);
      }}else{{
        const activeEnd=task.start+task.upper,streamFed=task.finish>activeEnd+.001,lower=Math.max(0,task.lower==null?task.upper:task.lower),solid=Math.max(.8,lower/extent*100),total=Math.max(solid,task.upper/extent*100);
        const bar=document.createElement('div');bar.className='gantt-bar '+(streamFed?'envelope':str(task.temporal&&task.temporal.status)==='bounded'?'bounded':'');bar.style.left=`${{left}}%`;bar.style.width=`${{Math.max(solid,(streamFed?task.finish-task.start:task.upper)/extent*100)}}%`;
        const note=document.createElement('div');note.className='gantt-bar-label';note.style.left=`${{left}}%`;note.textContent=streamFed?`starts ${{formatElapsed(task.start)}} · ${{temporalRangeValue(task.temporal)}} active · drains through ≥${{formatElapsed(task.finish)}}`:`starts ${{formatElapsed(task.start)}} · ${{temporalRangeValue(task.temporal)}}`;
        track.append(bar);
        if(streamFed){{
          const active=document.createElement('div');active.className='gantt-density active';active.style.left=`${{left}}%`;active.style.width=`${{Math.max(.08,task.upper/extent*100)}}%`;
          const waiting=document.createElement('div');waiting.className='gantt-density waiting';waiting.style.left=`${{Math.min(100,activeEnd/extent*100)}}%`;waiting.style.width=`${{Math.max(.08,(task.finish-activeEnd)/extent*100)}}%`;track.append(waiting,active);
        }}else if(total>solid+.05){{
          const uncertainty=document.createElement('div');uncertainty.className='gantt-uncertainty';uncertainty.style.left=`${{left+solid}}%`;uncertainty.style.width=`${{total-solid}}%`;track.append(uncertainty);
        }}
        track.append(note);
      }}
      row.append(label,track);row.title=[task.id,...task.streamNotes].join('\n');row.onclick=()=>{{selected=task.node;renderDetails()}};gantt.append(row);rowsById.set(task.id,row);
    }});
    timeline.append(gantt);
    const links=[];for(const task of tasks)for(const predecessor of predecessors.get(task.id)){{const source=byTask.get(predecessor.source);if(!source)continue;const repeated=Boolean(source.scheduleKey&&source.scheduleKey===task.scheduleKey),duration=repeated?exactNumber(source.scheduled&&source.scheduled.stage.duration_seconds):null,sourceTime=predecessor.kind==='handoff'?source.start+predecessor.delay:repeated&&duration!=null?source.start+duration:null;links.push({{source:source.id,target:task.id,repeated,sourceTime,label:predecessor.label}})}}requestAnimationFrame(()=>drawGanttLinks(gantt,rowsById,links,extent));
  }}
  function setViewMode(mode) {{
    activeView=mode==='timeline'?'timeline':'graph';const isTimeline=activeView==='timeline';svg.classList.toggle('hidden',isTimeline);timeline.classList.toggle('hidden',!isTimeline);document.getElementById('graphView').classList.toggle('active',!isTimeline);document.getElementById('graphView').setAttribute('aria-pressed',String(!isTimeline));document.getElementById('timelineView').classList.toggle('active',isTimeline);document.getElementById('timelineView').setAttribute('aria-pressed',String(isTimeline));document.getElementById('zoomOut').classList.toggle('hidden',isTimeline);document.getElementById('zoomIn').classList.toggle('hidden',isTimeline);document.getElementById('fit').classList.toggle('hidden',isTimeline);document.getElementById('ports').classList.toggle('hidden',isTimeline);document.querySelector('.legend').classList.toggle('hidden',isTimeline);if(isTimeline&&current)renderTimeline();
  }}
  function setView(v) {{ view=v; svg.setAttribute('viewBox',`${{v.x}} ${{v.y}} ${{v.w}} ${{v.h}}`); }}
  function fit() {{ const b=layoutBounds,pad=55; const host=svg.getBoundingClientRect(), aspect=Math.max(.25,host.width/Math.max(1,host.height)); let w=b.w+pad*2,h=b.h+pad*2; if(w/h<aspect)w=h*aspect;else h=w/aspect; setView({{x:b.x-(w-b.w)/2,y:b.y-(h-b.h)/2,w,h}}); }}
  function zoom(factor,cx=.5,cy=.5) {{ const nw=view.w*factor,nh=view.h*factor; setView({{x:view.x+(view.w-nw)*cx,y:view.y+(view.h-nh)*cy,w:nw,h:nh}}); }}
  function topology(nodes, edges) {{
    const ordered=[...nodes].sort((a,b)=>str(a.path||a.id).localeCompare(str(b.path||b.id)));
    const ids=new Set(ordered.map(n=>str(n.id))),byId=new Map(ordered.map(n=>[str(n.id),n])); const incoming=new Map(), outgoing=new Map();
    ordered.forEach(n=>{{incoming.set(str(n.id),0);outgoing.set(str(n.id),new Set())}});
    const link=(source,target)=>{{if(!ids.has(source)||!ids.has(target)||source===target||outgoing.get(source).has(target))return;outgoing.get(source).add(target);incoming.set(target,incoming.get(target)+1)}};
    // A composite's input and export bridges point in opposite data-flow
    // directions, so treating both as ordinary topological edges creates a
    // false cycle. Containment gives every child a stable rank after its
    // parent; only non-boundary wires constrain sibling execution order.
    ordered.forEach(node=>{{const parent=str(node.parent);if(parent)link(parent,str(node.id))}});
    edges.forEach(edge=>{{const source=str(edge.source),target=str(edge.target),sourceNode=byId.get(source),targetNode=byId.get(target);if(!sourceNode||!targetNode)return;if(str(sourceNode.parent)===target||str(targetNode.parent)===source)return;link(source,target)}});
    let queue=ordered.filter(n=>incoming.get(str(n.id))===0).map(n=>str(n.id)).sort(); const level=new Map(queue.map(id=>[id,0]));
    while(queue.length){{const id=queue.shift();for(const target of [...outgoing.get(id)].sort()){{level.set(target,Math.max(level.get(target)||0,(level.get(id)||0)+1));incoming.set(target,incoming.get(target)-1);if(incoming.get(target)===0){{queue.push(target);queue.sort()}}}}}}
    // Cycles remain visible in a final deterministic layer.
    const max=Math.max(0,...level.values()); ordered.forEach(n=>{{if(!level.has(str(n.id)))level.set(str(n.id),max+1)}});
    const layers=[]; ordered.forEach(n=>{{const l=level.get(str(n.id));(layers[l]||(layers[l]=[])).push(n)}}); return layers;
  }}
  function unpack(value) {{
    if(!value || typeof value!=='object' || !value.graph) return value;
    const report=value.report&&typeof value.report==='object'?value.report:{{}};
    const graph=value.graph&&typeof value.graph==='object'?value.graph:{{}};
    const merged={{...report,...graph}};
    if(value.error) merged.diagnostics=[...arr(merged.diagnostics),{{severity:'error',message:str(value.error)}}];
    return merged;
  }}
  function showServerError(message) {{
    const banner=document.getElementById('banner');
    banner.textContent='Live reload error: '+str(message);
    banner.classList.remove('hidden');
  }}
  function showStaleGraph() {{
    const banner=document.getElementById('banner');
    banner.textContent='The source is invalid; showing the last valid graph. See diagnostics for details.';
    banner.classList.remove('hidden');
  }}
  function draw(doc) {{
    doc=unpack(doc);
    current=doc&&typeof doc==='object'?doc:{{}}; selected=null; nodesLayer.replaceChildren(); edgesLayer.replaceChildren();
    const nodes=arr(current.nodes),edges=arr(current.edges),byId=new Map(nodes.map(node=>[str(node.id),node])),entry=str(current.entry||current.entry_path||'');
    nodeMetrics=associateMetrics(nodes,arr(current.monitored_metrics));temporalMetrics=new Map(arr(current.temporal_metrics).map(metric=>[str(metric.path),metric]));streamHandoffs=arr(current.stream_handoffs);streamCardinalities=arr(current.stream_cardinalities);resourceSchedules=arr(current.resource_schedules);provisionedResources=arr(current.provisioned_resources);
    document.getElementById('entry').textContent=entry; const status=str(current.status||((arr(current.diagnostics).some(d=>str(d.severity).toLowerCase()==='error'))?'invalid':'complete'));
    const badge=document.getElementById('status');badge.textContent=status;badge.className='status '+status.toLowerCase();
    document.title=(entry?entry+' · ':'')+'freql graph'; renderReport();renderTimeline();
    const banner=document.getElementById('banner');
    if(!nodes.length){{ banner.textContent=arr(current.diagnostics).length?'The model could not produce a run graph. See diagnostics.':'No run instances to display.';banner.classList.remove('hidden');layoutBounds={{x:0,y:0,w:700,h:420}};fit();return; }}
    banner.classList.add('hidden');
    const layers=topology(nodes,edges), W=250,BASE_H=82,METRIC_ROW=18,PORT_ROW=20,X=125,Y=75,positions=new Map(); let maxColumnHeight=BASE_H;
    layers.forEach((layer,col)=>{{let y=40;layer.forEach(node=>{{const ports=nodePorts(node),metrics=metricsForNode(node),metricLines=Math.min(metrics.length,2)+(metrics.length>2?1:0),metricHeight=metricLines?8+metricLines*METRIC_ROW:0,portTop=BASE_H+metricHeight,rows=Math.max(ports.inputs.length,ports.outputs.length),h=portTop+(rows?10+rows*PORT_ROW:0);const anchors=new Map();ports.inputs.forEach((member,index)=>anchors.set(str(member.name),{{x:0,y:portTop+10+index*PORT_ROW,side:'input'}}));ports.outputs.forEach((member,index)=>anchors.set(str(member.name),{{x:W,y:portTop+10+index*PORT_ROW,side:'output'}}));positions.set(str(node.id),{{x:40+col*(W+X),y,w:W,h,ports,metrics,metricLines,portTop,anchors}});y+=h+Y}});maxColumnHeight=Math.max(maxColumnHeight,y-Y-40)}});
    const cols=Math.max(1,layers.length);layoutBounds={{x:20,y:20,w:cols*W+Math.max(0,cols-1)*X+40,h:maxColumnHeight+80}};
    [...edges].sort((a,b)=>str(a.id||a.source+a.target).localeCompare(str(b.id||b.source+b.target))).forEach(edge=>{{
      const a=positions.get(str(edge.source)),b=positions.get(str(edge.target));if(!a||!b)return;const kind=edgeKind(edge),sourceAnchor=kind==='wire'&&edge.source_port?a.anchors.get(str(edge.source_port)):null,targetAnchor=kind==='wire'&&edge.target_port?b.anchors.get(str(edge.target_port)):null,sx=a.x+(sourceAnchor?sourceAnchor.x:a.w),sy=a.y+(sourceAnchor?sourceAnchor.y:a.h/2),tx=b.x+(targetAnchor?targetAnchor.x:0),ty=b.y+(targetAnchor?targetAnchor.y:b.h/2),mx=(sx+tx)/2;
      const path=svgEl('path',{{d:`M ${{sx}} ${{sy}} C ${{mx}} ${{sy}}, ${{mx}} ${{ty}}, ${{tx}} ${{ty}}`,class:'edge '+kind,'marker-end':`url(#arrow-${{kind}})`}}); edgesLayer.append(path);
      const cardinality=streamCardinalityForEdge(edge,byId),edgeParts=[edge.label,edge.ty,cardinality&&`${{str(cardinality.cardinality)}} items`,edge.ownership].filter(Boolean);const label=edgeParts.map(str).join(' · ');if(label)edgesLayer.append(textEl(mx,(sy+ty)/2-6,label,'edge-label'));
    }});
    nodes.forEach(node=>{{const p=positions.get(str(node.id));const g=svgEl('g',{{class:'node',tabindex:'0',role:'button','aria-label':str(node.path||node.label||node.id),transform:`translate(${{p.x}} ${{p.y}})`}});g.dataset.id=str(node.id);g.append(svgEl('rect',{{width:p.w,height:p.h,rx:10}}));g.append(textEl(14,25,node.label||String(node.path||node.id).split('.').pop(),'title'));g.append(textEl(14,46,node.process||node.type||node.path||node.id,'sub'));g.append(textEl(14,68,node.kind||'run','kind'));if(p.metricLines){{g.append(svgEl('line',{{x1:0,y1:BASE_H,x2:p.w,y2:BASE_H,class:'metric-rule'}}));p.metrics.slice(0,2).forEach((entry,index)=>{{const label=textEl(14,BASE_H+15+index*METRIC_ROW,metricSummary(entry),'metric-text'),title=svgEl('title');title.textContent=`${{str(entry.metric.path)}}: ${{monitoredValue(entry.metric)}} (${{str(entry.metric.ty)}}, ${{str(entry.metric.aggregate)}})`;label.append(title);g.append(label)}});if(p.metrics.length>2)g.append(textEl(14,BASE_H+15+2*METRIC_ROW,`+${{p.metrics.length-2}} more monitored`, 'metric-more'))}}if(p.ports.inputs.length||p.ports.outputs.length){{g.append(svgEl('line',{{x1:0,y1:p.portTop,x2:p.w,y2:p.portTop,class:'port-rule'}}));const addPort=(member,index,side)=>{{const y=p.portTop+10+index*PORT_ROW,role=portRole(member),circle=svgEl('circle',{{cx:side==='input'?0:p.w,cy:y,r:4,class:'port-dot '+role}}),label=textEl(side==='input'?10:p.w-10,y+3,shortPort(member.name),'port-name');if(side==='output')label.setAttribute('text-anchor','end');const title=svgEl('title');title.textContent=`${{role}} ${{str(member.name)}}: ${{str(member.ty)}}`;circle.append(title);g.append(circle,label)}};p.ports.inputs.forEach((member,index)=>addPort(member,index,'input'));p.ports.outputs.forEach((member,index)=>addPort(member,index,'output'))}}const choose=()=>selectNode(node,g);g.addEventListener('click',choose);g.addEventListener('keydown',e=>{{if(e.key==='Enter'||e.key===' '){{e.preventDefault();choose()}}}});nodesLayer.append(g)}});
    fit();
  }}
  function selectNode(node, element) {{ selected=node;document.querySelectorAll('.node.selected').forEach(x=>x.classList.remove('selected'));element.classList.add('selected');renderDetails(); }}
  function appendMetricReading(root,presentation) {{
    if(presentation.symbolic){{const unavailable=document.createElement('div');unavailable.className='metric-uncomputed';unavailable.textContent='Not yet computable';root.append(unavailable);return}}
    const reading=document.createElement('div');reading.className='metric-reading';const parts=quantityParts(presentation.value,presentation.decimal);const number=document.createElement('span');number.className='metric-number';number.textContent=parts.number;reading.append(number);if(parts.unit){{const unit=document.createElement('span');unit.className='metric-unit';unit.textContent=parts.unit;reading.append(unit)}}root.append(reading);
  }}
  function appendMetricDependencies(root,metric,presentation) {{
    if(!presentation.symbolic)return;const dependencies=[...new Set(arr(metric&&metric.dependencies).map(str).filter(Boolean))];const label=document.createElement('div');label.className='metric-dependency-label';label.textContent=dependencies.length?'Depends on:':'Depends on unresolved input';root.append(label);if(!dependencies.length)return;const list=document.createElement('ul');list.className='metric-dependencies';for(const dependency of dependencies.slice(0,5)){{const item=document.createElement('li');item.textContent=dependency;list.append(item)}}if(dependencies.length>5){{const item=document.createElement('li');item.textContent=`+${{dependencies.length-5}} more`;list.append(item)}}root.append(list);
  }}
  function metricTechnicalDisclosure(metric,temporal) {{
    const disclosure=document.createElement('details');disclosure.className='metric-technical';const summary=document.createElement('summary');summary.textContent='Show technical details';disclosure.append(summary);const details=document.createElement('dl');details.className='pairs';const source=metric.span&&metric.span.path?`${{metric.span.path}}:${{metric.span.line||1}}:${{metric.span.column||1}}`:'',direct=metric.direct_value==null?null:truncateMonitoredText(metric.direct_value);for(const [name,detail] of [['path',metric.path],['raw value',metric.value],['type',metric.ty],['display unit',metric.display_unit],['aggregation',metric.aggregate],['temporal status',temporal&&temporal.status],['known-work lower bound',temporal&&temporal.known_work&&temporal.known_work.lower],['known-work upper bound',temporal&&temporal.known_work&&temporal.known_work.upper],['completion lower bound',temporal&&temporal.completion_lower],['completion upper bound',temporal&&temporal.completion_upper],['direct contribution',direct],['direct expression',metric.direct_expression],['total expression',metric.expression],['unresolved dependencies',arr(metric.dependencies).join(', ')],['source',source]]){{if(detail==null||detail==='')continue;const dt=document.createElement('dt'),dd=document.createElement('dd');dt.textContent=name;dd.textContent=str(detail);dd.className='mono';details.append(dt,dd)}}if(temporal&&arr(temporal.obligations).length){{const dt=document.createElement('dt'),dd=document.createElement('dd');dt.textContent='open duration obligations';dd.textContent=arr(temporal.obligations).join(', ');dd.className='mono';details.append(dt,dd)}}disclosure.append(details);return disclosure;
  }}
  function renderDetails() {{
    const root=document.getElementById('details');root.replaceChildren();if(!selected){{root.innerHTML='<h2>Run details</h2><div class="empty">Select a node to inspect its process, configuration, and invocation.</div>';return}}
    let scheduled,claims,parallel,utilization,availability;
    const h=document.createElement('h2');h.textContent=selected.id||selected.path||selected.label;root.append(h);const dl=document.createElement('dl');dl.className='pairs';
    for(const [name,value] of [['instance',selected.id],['process',selected.process],['declaration',selected.process_path],['kind',selected.kind],['parent',selected.parent],['bodyless',selected.bodyless],['composite',selected.composite]]){{if(value==null)continue;const dt=document.createElement('dt'),dd=document.createElement('dd');dt.textContent=name;dd.textContent=str(value);dd.className='mono';dl.append(dt,dd)}}root.append(dl);
    for(const [title,value] of [['Configuration',selected.config||selected.configuration],['Configuration sources',selected.config_sources],['Invocation',selected.arguments||selected.invocation],['Interface',selected.members]]){{if(value==null||((Array.isArray(value)||typeof value==='object')&&Object.keys(value).length===0))continue;const h3=document.createElement('h3');h3.textContent=title;root.append(h3);const pre=document.createElement('div');pre.className='item mono';pre.textContent=typeof value==='string'?value:JSON.stringify(value,null,2);root.append(pre)}}
    const candidates=nodePathCandidates(selected),resources=provisionedResources.filter(resource=>[resource.path,...arr(resource.aliases),...arr(resource.borrowers)].some(endpoint=>candidates.some(candidate=>str(endpoint)===candidate||str(endpoint).startsWith(candidate+'.'))));if(resources.length){{const h3=document.createElement('h3');h3.textContent=`Resources (${{resources.length}})`;root.append(h3);for(const resource of resources){{const card=document.createElement('div');card.className='item resource-detail';const pairs=document.createElement('dl');pairs.className='pairs',scheduled=resourceSchedules.flatMap(schedule=>arr(schedule.resources)).filter(value=>str(value.path)===str(resource.path)),claims=[...new Set(scheduled.map(value=>`${{str(value.stage).split('.').pop()}}: ${{str(value.claim_per_operation)}}`).filter(Boolean))].join(', '),parallel=[...new Set(scheduled.map(value=>str(value.parallel_capacity)).filter(Boolean))].join(', '),utilization=exactNumber(resource.utilization),availability=exactNumber(resource.available_after_seconds);for(const [name,value] of [['origin',resource.path],['type',resource.resource],['capacity',resource.capacity],['available after',availability==null?'':formatElapsed(availability)],['claims',claims],['parallel capacities',parallel],['aggregate utilization',utilization==null?resource.utilization:`${{(utilization*100).toFixed(3)}}%`],['aliases',arr(resource.aliases).join(', ')],['borrowers',arr(resource.borrowers).join(', ')]]){{if(value==null||value==='')continue;const dt=document.createElement('dt'),dd=document.createElement('dd');dt.textContent=name;dd.textContent=str(value);dd.className='mono';pairs.append(dt,dd)}}card.append(pairs);root.append(card)}}}}
    const monitored=metricsForNode(selected);if(monitored.length){{const h3=document.createElement('h3');h3.textContent='Monitored metrics';root.append(h3);for(const entry of monitored){{const metric=entry.metric||{{}},temporal=temporalForMetric(metric),presentation=metricPresentation(metric),card=document.createElement('div');card.className='item metric-detail';appendMetricReading(card,presentation);appendMetricDependencies(card,metric,presentation);if(presentation.reason){{const status=document.createElement('div');status.className='metric-status';status.textContent=str(presentation.reason);card.append(status)}}card.append(metricTechnicalDisclosure(metric,temporal));root.append(card)}}}}
  }}
  function renderMetricCards(root,metrics) {{
    root.replaceChildren();const heading=document.createElement('h3');heading.textContent=`Monitored metrics (${{metrics.length}})`;root.append(heading);
    if(!metrics.length){{const empty=document.createElement('div');empty.className='empty';empty.textContent='No monitored values on this model.';root.append(empty);return}}
    const list=document.createElement('div');list.className='metric-list';
    metrics.forEach(metric=>{{
      const presentation=metricPresentation(metric),card=document.createElement('article');card.className='metric-card'+(presentation.incomplete?' incomplete':metric&&metric.bounds?' bounded':'');card.title=str(metric&&metric.path);
      const head=document.createElement('div');head.className='metric-card-head';const label=document.createElement('div');label.className='metric-label';label.textContent=humanize(terminalMetricName(metric));const context=document.createElement('div');context.className='metric-context';context.textContent=metricPathContext(metric);context.title=str(metric&&metric.path);head.append(label,context);card.append(head);
      appendMetricReading(card,presentation);appendMetricDependencies(card,metric,presentation);
      if(presentation.reason){{const status=document.createElement('div');status.className='metric-status';status.textContent=str(presentation.reason);card.append(status)}}if(presentation.symbolic)card.append(metricTechnicalDisclosure(metric,temporalForMetric(metric)));list.append(card);
    }});root.append(list);
  }}
  function appendReportGroup(root,name,items) {{
    const h=document.createElement('h3');h.textContent=`${{name}} (${{items.length}})`;root.append(h);
    items.forEach(value=>{{const item=document.createElement('div');const severity=str(value&&value.severity).toLowerCase();item.className='item '+severity;item.textContent=itemText(value);if(value&&typeof value==='object'){{const where=value.path||(value.span&&value.span.path?`${{value.span.path}}:${{value.span.line||1}}:${{value.span.column||1}}`:'');const tags=[value.code,value.phase,value.status,value.bounds&&value.bounds.reason].filter(Boolean).map(str);const meta=[where,...tags].filter(Boolean).join(' · ');if(meta){{const line=document.createElement('div');line.className='mono';line.textContent=meta;item.append(line)}}}}root.append(item)}});
  }}
  async function applyOverride(path,value,resetAll=false) {{
    const state=document.getElementById('liveState');state.textContent='updating…';
    try {{
      const response=await fetch('/api/overrides',{{method:'POST',headers:{{'content-type':'application/json'}},body:JSON.stringify(resetAll?{{reset_all:true}}:{{path,value}})}});
      const payload=await response.json();
      if(!response.ok)throw new Error(payload.server_error||`override request failed (${{response.status}})`);
      draw(payload.document||payload);if(payload.stale_graph)showStaleGraph();state.textContent='live';
    }} catch(error) {{ state.textContent='update error';showServerError(error.message) }}
  }}
  function renderControls() {{
    const root=document.getElementById('controls');root.replaceChildren();
    if(!LIVE){{root.classList.add('hidden');return}}root.classList.remove('hidden');
    const controls=arr(current.controls),heading=document.createElement('div');heading.className='control-heading';
    const needle=controlFilter.trim().toLowerCase(),filtered=controls.filter(control=>!needle||[control.name,control.path,control.ty].some(value=>str(value).toLowerCase().includes(needle)));
    const title=document.createElement('h3');title.textContent=needle?`Parameters (${{filtered.length}} of ${{controls.length}})`:`Parameters (${{controls.length}})`;heading.append(title);
    if(controls.some(control=>control.overridden)||Object.keys(current.overrides||{{}}).length){{const resetAll=document.createElement('button');resetAll.textContent='Reset all';resetAll.title='Remove every browser override';resetAll.onclick=()=>applyOverride(null,null,true);heading.append(resetAll)}}root.append(heading);
    if(!controls.length){{const empty=document.createElement('div');empty.className='empty';empty.textContent='No exposed configuration on this process.';root.append(empty);return}}
    if(controls.length>8){{const search=document.createElement('input');search.className='control-search';search.type='search';search.placeholder='Filter parameters';search.value=controlFilter;search.setAttribute('aria-label','Filter parameters');search.addEventListener('input',()=>{{controlFilter=search.value;renderControls()}});root.append(search)}}
    if(!filtered.length){{const empty=document.createElement('div');empty.className='empty';empty.textContent='No parameters match this filter.';root.append(empty);return}}
    filtered.forEach(control=>{{
      const card=document.createElement('div');card.className='control'+(control.overridden?' overridden':'');
      const titleRow=document.createElement('div');titleRow.className='control-title-row';const label=document.createElement('label');label.className='control-name';label.textContent=str(control.name);titleRow.append(label);if(control.overridden){{const badge=document.createElement('span');badge.className='control-badge';badge.textContent='changed';titleRow.append(badge)}}card.append(titleRow);
      const meta=document.createElement('div');meta.className='control-meta';const type=document.createElement('span');type.className='control-type';type.textContent=str(control.ty);const path=document.createElement('span');path.className='control-path';path.textContent=str(control.path);path.title=str(control.path);meta.append(type,path);card.append(meta);
      const row=document.createElement('div');row.className='control-row';const input=document.createElement('input');input.type='text';input.value=str(control.value);input.spellcheck=false;input.setAttribute('aria-label',str(control.path));
      input.addEventListener('input',()=>{{const prior=controlTimers.get(control.path);if(prior)clearTimeout(prior);controlTimers.set(control.path,setTimeout(()=>{{controlTimers.delete(control.path);applyOverride(control.path,input.value)}},300))}});row.append(input);
      if(control.overridden){{const reset=document.createElement('button');reset.textContent='Reset';reset.title=control.default?`Restore default: ${{str(control.default)}}`:'Clear this override';reset.onclick=()=>applyOverride(control.path,null);row.append(reset)}}card.append(row);root.append(card);
    }});
  }}
  function renderReport() {{
    renderDetails();
    renderControls();
    const metricsRoot=document.getElementById('metrics');renderMetricCards(metricsRoot,compilationMetrics(arr(current.monitored_metrics)));
    const root=document.getElementById('report');root.replaceChildren();const groups=[['Provisioned resources',arr(current.provisioned_resources)],['Schedules',arr(current.resource_schedules)],['Diagnostics',arr(current.diagnostics)],['Obligations',arr(current.obligations)],['Residual constraints',arr(current.constraints||current.residual_constraints)]];
    if(current.report_scope){{const h=document.createElement('h3');h.textContent='Report scope';root.append(h);const scope=document.createElement('div');scope.className='item mono';scope.textContent=str(current.report_scope);root.append(scope)}}
    groups.forEach(([name,items])=>appendReportGroup(root,name,items));
  }}
  parametersToggle.hidden=!LIVE;parametersToggle.onclick=()=>setParametersOpen(!parametersOpen);setParametersOpen(parametersOpen,false);
  document.addEventListener('keydown',event=>{{if(event.key==='Escape'&&parametersOpen&&matchMedia('(max-width:1100px)').matches){{setParametersOpen(false);parametersToggle.focus()}}}});
  document.getElementById('graphView').onclick=()=>setViewMode('graph');document.getElementById('timelineView').onclick=()=>setViewMode('timeline');document.getElementById('zoomIn').onclick=()=>zoom(.8);document.getElementById('zoomOut').onclick=()=>zoom(1.25);document.getElementById('fit').onclick=fit;document.getElementById('ports').onclick=event=>{{portsVisible=!portsVisible;event.currentTarget.textContent=portsVisible?'Ports: on':'Ports: off';event.currentTarget.setAttribute('aria-pressed',String(portsVisible));event.currentTarget.classList.toggle('active',portsVisible);if(current)draw(current)}};
  svg.addEventListener('wheel',e=>{{e.preventDefault();const r=svg.getBoundingClientRect();zoom(e.deltaY>0?1.12:.89,(e.clientX-r.left)/r.width,(e.clientY-r.top)/r.height)}},{{passive:false}});
  svg.addEventListener('pointerdown',e=>{{if(e.target.closest('.node'))return;svg.setPointerCapture(e.pointerId);dragging={{x:e.clientX,y:e.clientY,v:{{...view}}}}}});svg.addEventListener('pointermove',e=>{{if(!dragging)return;const r=svg.getBoundingClientRect();setView({{...view,x:dragging.v.x-(e.clientX-dragging.x)*dragging.v.w/r.width,y:dragging.v.y-(e.clientY-dragging.y)*dragging.v.h/r.height}})}});svg.addEventListener('pointerup',()=>dragging=null);svg.addEventListener('pointercancel',()=>dragging=null);
  try {{ const initial=JSON.parse(document.getElementById('freql-document').textContent);if(initial){{if(initial.server_error){{if(initial.nodes||initial.document||initial.graph)draw(initial.document||initial.graph||initial);showServerError(initial.server_error)}}else{{draw(initial);if(initial.stale_graph)showStaleGraph()}}}}else if(!LIVE)draw({{}}) }} catch(error) {{ draw({{status:'invalid',diagnostics:[{{severity:'error',message:'Could not decode graph: '+error.message}}]}}) }}
  if(LIVE){{const state=document.getElementById('liveState');state.textContent='connecting…';const source=new EventSource('/events');const receive=event=>{{try{{const value=JSON.parse(event.data);if(value.server_error){{if(value.nodes||value.document||value.graph)draw(value.document||value.graph||value);showServerError(value.server_error);state.textContent='reload error';return}}draw(value.document||value);if(value.stale_graph)showStaleGraph();state.textContent='live'}}catch(error){{state.textContent='bad update';showServerError(error.message)}}}};source.onmessage=receive;source.addEventListener('graph',receive);source.onopen=()=>{{if(!current)state.textContent='connected, waiting…'}};source.onerror=()=>state.textContent='reconnecting…'}}
}})();
</script>
</body>
</html>
"####
    )
}

#[cfg(test)]
mod tests {
    use super::{escape_embedded_json, live_html, live_html_with_initial, render_page};

    #[test]
    fn output_is_self_contained() {
        let html = render_page(Some(r#"{"nodes":[],"edges":[]}"#), false);
        assert!(html.starts_with("<!doctype html>"));
        assert!(!html.contains("src=\"http"));
        assert!(!html.contains("href=\"http"));
        assert!(html.contains("const LIVE = false"));
    }

    #[test]
    fn embedded_json_cannot_end_script_element() {
        let escaped = escape_embedded_json(r#"{"label":"</script><script>alert(1)</script>"}"#);
        assert!(!escaped.to_ascii_lowercase().contains("</script"));
        assert!(escaped.contains("\\u003c/script\\u003e"));
    }

    #[test]
    fn live_shell_has_sse_and_graph_controls() {
        let html = live_html();
        assert!(html.contains("new EventSource('/events')"));
        assert!(html.contains("id=\"zoomIn\""));
        assert!(html.contains("id=\"zoomOut\""));
        assert!(html.contains("id=\"fit\""));
        assert!(html.contains("id=\"ports\""));
        assert!(html.contains("Ports: on"));
        assert!(html.contains("id=\"timelineView\""));
        assert!(html.contains(">Gantt</button>"));
        assert!(html.contains("gantt-axis"));
        assert!(html.contains("gantt-bar"));
        assert!(html.contains("temporalSeconds"));
        assert!(html.contains("edge.ty).startsWith('Stream[')"));
        assert!(html.contains("streamHandoffForEdge"));
        assert!(html.contains("streamCardinalityForEdge"));
        assert!(html.contains("streamCardinalities=arr(current.stream_cardinalities)"));
        assert!(html.contains("${str(cardinality.cardinality)} items"));
        assert!(html.contains("scheduleForNode"));
        assert!(html.contains("prior.scheduleKey===task.scheduleKey"));
        assert!(html.contains("scheduledStart+offset"));
        assert!(html.contains("task.scheduled.stage.maximum_duration_seconds"));
        assert!(html.contains("drawGanttLinks"));
        assert!(html.contains("gantt-link"));
        assert!(html.contains("provisionedResources=arr(current.provisioned_resources)"));
        assert!(html.contains("const taskForInstance=path"));
        assert!(html.contains("resource:true"));
        assert!(!html.contains("Bars use explicit causal edges"));
        assert!(html.contains("nodePathCandidates(node)"));
        assert!(html.contains("kind:'handoff'"));
        assert!(html.contains("kind:'stream-close'"));
        assert!(html.contains("attachedMetrics=metricsForNode(task.node)"));
        assert!(html.contains("let attachedMetrics,stageMin,stageMax,stageDuration,baseMeta;"));
        assert!(html.contains("let scheduled,claims,parallel,utilization,availability;"));
        assert!(html.contains("label:predecessor.label"));
        assert!(html.contains("stage lifetime"));
        assert!(html.contains("active operations"));
        assert!(html.contains("waiting / draining"));
        assert!(html.contains("queued / backlog"));
        assert!(html.contains("average_active_operations"));
        assert!(html.contains("renderActivityDensity"));
        assert!(html.contains("first_item_after_seconds"));
        assert!(html.contains("edge.source_port"));
        assert!(html.contains("edge.target_port"));
        assert!(html.contains("['gets','uses']"));
        assert!(html.contains("portRole(member)==='export'"));
        assert!(html.contains("physical transfer"));
        assert!(html.contains("causal order"));
        assert!(html.contains("showStaleGraph"));
        assert!(html.contains("value.server_error"));
        assert!(html.contains("id=\"parametersToggle\""));
        assert!(html.contains("aria-controls=\"parameterPanel\""));
        assert!(html.contains("id=\"parameterPanel\" class=\"parameter-panel\""));
        assert!(html.contains("id=\"controls\""));
        assert!(html.contains("sessionStorage.getItem('freql.parameters.open')"));
        assert!(html.contains("sessionStorage.setItem('freql.parameters.open'"));
        assert!(html.contains("parameterPanel.toggleAttribute('inert',!parametersOpen)"));
        assert!(html.contains("matchMedia('(max-width:1100px)')"));
        assert!(html.contains("fetch('/api/overrides'"));
        assert!(html.contains("Parameters (${controls.length})"));
        assert!(html.contains("setTimeout(()=>"));
        assert!(html.contains("reset_all:true"));
        assert!(html.contains("Resources (${resources.length})"));
        assert!(html.contains("['capacity',resource.capacity]"));
        assert!(html.contains("['claims',claims]"));
        assert!(html.contains("['aggregate utilization'"));
        assert!(html.contains("resource.available_after_seconds"));
        assert!(html.contains("containingScheduleOffset"));
        assert!(html.contains("['Provisioned resources',arr(current.provisioned_resources)]"));
    }

    #[test]
    fn topology_treats_composite_bridges_as_containment_not_cycles() {
        let html = render_page(
            Some(
                r#"{
                    "nodes":[
                        {"id":"main.req::Entry","kind":"process"},
                        {"id":"main.req::Entry.batch","kind":"run","parent":"main.req::Entry"}
                    ],
                    "edges":[
                        {"source":"main.req::Entry","target":"main.req::Entry.batch","kind":"physical"},
                        {"source":"main.req::Entry.batch","target":"main.req::Entry","kind":"physical"}
                    ]
                }"#,
            ),
            false,
        );

        assert!(html.contains("ordered.forEach(node=>{const parent=str(node.parent)"));
        assert!(html.contains("str(sourceNode.parent)===target"));
        assert!(html.contains("str(targetNode.parent)===source"));
    }

    #[test]
    fn graph_nodes_render_associated_monitored_metrics() {
        let html = render_page(
            Some(
                r#"{
                    "nodes":[
                        {"id":"main.req::Factory","kind":"process"},
                        {"id":"main.req::Factory.leaf","kind":"run"}
                    ],
                    "edges":[],
                    "monitored_metrics":[{
                        "path":"Factory.leaf.material",
                        "ty":"Mass",
                        "value":"2 Kilogram",
                        "aggregate":"sum"
                    }]
                }"#,
            ),
            false,
        );

        assert!(html.contains("const associateMetrics"));
        assert!(html.contains("prefix.length>best.prefix.length"));
        assert!(html.contains("kind!=='run'&&kind!=='process'"));
        assert!(html.contains("metricSummary(entry)"));
        assert!(html.contains("className='item metric-detail'"));
        assert!(html.contains("['aggregation',metric.aggregate]"));
        assert!(html.contains(
            "['known-work lower bound',temporal&&temporal.known_work&&temporal.known_work.lower]"
        ));
        assert!(html.contains("['completion upper bound',temporal&&temporal.completion_upper]"));
        assert!(html.contains("['direct contribution',direct]"));
        assert!(html.contains("['total expression',metric.expression]"));
        assert!(html.contains("item.predicate || (item.makespan != null"));
        assert!(html.contains(r#""path":"Factory.leaf.material""#));
        assert!(html.contains(r#""value":"2 Kilogram""#));
    }

    #[test]
    fn monitored_metric_values_are_truncated_only_in_web_presentation() {
        let html = render_page(
            Some(
                r#"{
                    "nodes":[],
                    "edges":[],
                    "monitored_metrics":[{
                        "path":"Factory.cost",
                        "value":"12.34567890123456789 USD"
                    }]
                }"#,
            ),
            false,
        );

        // The exact report payload remains embedded for inspection and APIs.
        assert!(html.contains(r#""value":"12.34567890123456789 USD""#));
        // Every monitored-value presentation path uses the truncating helper.
        assert!(html.contains("const truncateMonitoredText"));
        assert!(html.contains("fraction.slice(0,3)"));
        assert!(html.contains("const monitoredValue"));
        assert!(html.contains("quantityParts(presentation.value,presentation.decimal)"));
        assert!(html.contains("number.textContent=parts.number"));
        assert!(html.contains("scales[0]||presentation.value"));
        assert!(html.contains("const metricPresentation"));
    }

    #[test]
    fn symbolic_metrics_show_only_unresolved_inputs_until_details_are_opened() {
        let html = render_page(
            Some(
                r#"{
                    "nodes":[],
                    "edges":[],
                    "monitored_metrics":[{
                        "path":"Factory.ship.duration",
                        "ty":"Time",
                        "value":"shipping_plan(current.package.length, 1000).duration",
                        "expression":"shipping_plan(current.package.length, 1000).duration",
                        "dependencies":["current.package.length"]
                    }]
                }"#,
            ),
            false,
        );

        // The exact expression stays in the embedded report and in a collapsed
        // technical disclosure; the default card is concise.
        assert!(html.contains("Not yet computable"));
        assert!(html.contains("Depends on:"));
        assert!(html.contains("Show technical details"));
        assert!(html.contains("presentation.symbolic"));
        assert!(html.contains("dependencies.slice(0,5)"));
        assert!(html.contains("['raw value',metric.value]"));
        assert!(html.contains(r#""dependencies":["current.package.length"]"#));
        assert!(html.contains(r#""value":"shipping_plan(current.package.length, 1000).duration""#));
    }

    #[test]
    fn unresolved_metric_bounds_render_as_a_compact_range_with_reason() {
        let html = render_page(
            Some(
                r#"{
                    "nodes":[],
                    "edges":[],
                    "monitored_metrics":[{
                        "path":"Factory.duration",
                        "value":"makespan(2 Day, 3 Day)",
                        "bounds":{
                            "lower":"3 Day",
                            "upper":"11/2 Day",
                            "reason":"unresolved pipeline makespan"
                        }
                    }]
                }"#,
            ),
            false,
        );

        assert!(html.contains("const metricBoundValue"));
        assert!(html.contains("lower.number}–${upper.number"));
        assert!(html.contains("status.className='metric-status'"));
        assert!(html.contains("value.bounds&&value.bounds.reason"));
        assert!(html.contains(r#""lower":"3 Day""#));
        assert!(html.contains(r#""reason":"unresolved pipeline makespan""#));
    }

    #[test]
    fn incomplete_temporal_metric_is_labelled_as_known_work() {
        let html = render_page(
            Some(
                r#"{
                    "nodes":[],
                    "edges":[],
                    "monitored_metrics":[{
                        "path":"Factory.duration",
                        "value":"2 Second",
                        "aggregate":"makespan"
                    }],
                    "temporal_metrics":[{
                        "path":"Factory.duration",
                        "known_work":{"lower":"2 Second","upper":"2 Second"},
                        "status":"incomplete",
                        "obligations":["Factory.untimed.duration"]
                    }]
                }"#,
            ),
            false,
        );

        assert!(html.contains("temporalMetrics=new Map(arr(current.temporal_metrics)"));
        assert!(html.contains("known work only · successful completion has"));
        assert!(html.contains("metric-card'+(presentation.incomplete?' incomplete'"));
        assert!(html.contains(r#""status":"incomplete""#));
        assert!(html.contains(r#""Factory.untimed.duration""#));
    }

    #[test]
    fn side_panels_separate_parameters_from_analysis_and_hoist_monitored_metrics() {
        let html = render_page(
            Some(
                r#"{
                    "nodes":[],
                    "edges":[],
                    "diagnostics":[],
                    "obligations":[{"message":"first"},{"message":"second"}],
                    "constraints":[{"predicate":"x > 0"}],
                    "monitored_metrics":[{"path":"Factory.cost","value":"12 USD"}]
                }"#,
            ),
            false,
        );

        assert!(html.contains(
            r#"<aside id="parameterPanel" class="parameter-panel" aria-label="Exposed parameters"><div id="controls"></div></aside>"#
        ));
        assert!(html.contains(
            r#"<aside class="inspector-panel" aria-label="Analysis and run details"><div id="metrics"></div><div id="details"></div><div id="report"></div></aside>"#
        ));
        assert!(html.contains("renderMetricCards(metricsRoot,compilationMetrics"));
        assert!(html.contains("className='metric-card'"));
        assert!(html.contains("const metricPathContext"));
        assert!(html.contains("h.textContent=`${name} (${items.length})`"));
        assert!(html.contains("['Diagnostics',arr(current.diagnostics)]"));
        assert!(html.contains("['Obligations',arr(current.obligations)]"));
        assert!(html.contains(
            "['Residual constraints',arr(current.constraints||current.residual_constraints)]"
        ));
        assert!(!html.contains("metric-grid"));
    }

    #[test]
    fn compilation_metric_summary_keeps_composite_totals_and_sibling_metrics() {
        let html = render_page(
            Some(
                r#"{
                    "nodes":[],
                    "edges":[],
                    "monitored_metrics":[
                        {"path":"Factory.cost","value":"30 USD"},
                        {"path":"Factory.batch.cost","value":"20 USD"},
                        {"path":"Factory.batch.cut.cost","value":"10 USD"},
                        {"path":"Factory.batch.upload.egress","value":"8 TerabitPerSecond"},
                        {"path":"Factory.batch.download.egress","value":"4 TerabitPerSecond"},
                        {"path":"Factory.batch.records","value":"1000"}
                    ]
                }"#,
            ),
            false,
        );

        assert!(html.contains("const terminalMetricName"));
        assert!(html.contains("const metricInstancePath"));
        assert!(html.contains("const isAncestorMetric"));
        assert!(html.contains("isAncestorMetric(other,metric)"));
        assert!(html.contains("Sibling fleet metrics are"));
        assert!(html.contains("compilationMetrics(arr(current.monitored_metrics))"));
        // The complete metric collection remains embedded for node association
        // and selected-node details; filtering happens only while rendering the
        // compilation-wide summary.
        assert!(html.contains(r#""path":"Factory.batch.cut.cost""#));
        assert!(html.contains("nodeMetrics=associateMetrics"));
        assert!(html.contains("['total expression',metric.expression]"));
        assert!(html.contains("['display unit',metric.display_unit]"));
    }

    #[test]
    fn live_shell_can_embed_an_initial_update() {
        let html = live_html_with_initial(r#"{"revision":7,"graph":{"nodes":[]}}"#);
        assert!(!html.contains("&quot;"));
        assert!(html.contains(r#""revision":7"#));
    }
}
