// graineo in the browser. Plain JavaScript, no framework, nothing from other servers.
// Talks only to the graineo that served this page (same origin, cookie set by pairing).
"use strict";

const $ = (id) => document.getElementById(id);
const state = {
  texts: {}, lang: "en", csrf: "", device: null,
  source: "all", q: "", minStars: 0, flag: "", label: 0, sort: "date_desc",
  items: [], total: 0, loading: false, gen: 0, sel: -1,
  loupe: -1, dev: null, renderBusy: false, renderAgain: false, renderTimer: 0, photoUrl: "",
};

// ---------- texts ----------
function t(key, vars) {
  let s = (state.texts[key] && (state.texts[key][state.lang] || state.texts[key].en)) || key;
  if (vars) for (const k in vars) s = s.replace("{" + k + "}", vars[k]);
  return s;
}
async function loadTexts() {
  const want = (navigator.languages || [navigator.language || "en"]).map((l) => l.toLowerCase());
  try {
    const r = await fetch("/texts.tsv", { credentials: "same-origin" });
    const lines = (await r.text()).split("\n");
    const head = lines[0].split("\t");
    for (const line of lines.slice(1)) {
      if (!line.trim() || line.startsWith("#")) continue;
      const c = line.split("\t");
      const e = {};
      head.slice(1).forEach((lang, i) => { e[lang] = c[i + 1]; });
      state.texts[c[0]] = e;
    }
    const langs = head.slice(1);
    state.lang = (want.map((w) => w.slice(0, 2)).find((w) => langs.includes(w))) || "en";
  } catch (_) { /* texts stay keys */ }
  document.documentElement.lang = state.lang;
  document.title = t("title");
  document.querySelectorAll("[data-t]").forEach((n) => { n.textContent = t(n.dataset.t); });
  document.querySelectorAll("[data-ph]").forEach((n) => { n.placeholder = t(n.dataset.ph); });
}

// ---------- helpers ----------
function el(tag, props, ...kids) {
  const n = document.createElement(tag);
  if (props) for (const k in props) {
    if (k === "class") n.className = props[k];
    else if (k === "text") n.textContent = props[k];
    else if (k.startsWith("on")) n.addEventListener(k.slice(2), props[k]);
    else if (k === "dataset") Object.assign(n.dataset, props[k]);
    else n.setAttribute(k, props[k]);
  }
  for (const c of kids) if (c != null) n.append(c);
  return n;
}
let toastTimer = 0;
function toast(msg, ms) {
  const n = $("toast");
  n.textContent = msg;
  n.classList.remove("hidden");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => n.classList.add("hidden"), ms || 3500);
}
function store(k, v) { try { localStorage.setItem("graineo." + k, JSON.stringify(v)); } catch (_) {} }
function recall(k, d) { try { const v = localStorage.getItem("graineo." + k); return v == null ? d : JSON.parse(v); } catch (_) { return d; } }

class HttpError extends Error { constructor(status, code) { super(code || String(status)); this.status = status; this.code = code; } }

async function api(method, path, body) {
  const opt = { method, credentials: "same-origin", headers: {} };
  if (method !== "GET") {
    opt.headers["Content-Type"] = "application/json";
    opt.headers["X-Graineo-CSRF"] = state.csrf;
    opt.body = JSON.stringify(body || {});
  }
  let r;
  try { r = await fetch(path, opt); } catch (_) { toast(t("offline"), 6000); throw new HttpError(0, "offline"); }
  if (r.status === 401 && path !== "/api/pair") { showPair(); throw new HttpError(401, "not_paired"); }
  const type = r.headers.get("Content-Type") || "";
  if (!r.ok) {
    let code = "";
    if (type.startsWith("application/json")) { try { code = (await r.json()).error; } catch (_) {} }
    throw new HttpError(r.status, code);
  }
  return type.startsWith("application/json") ? r.json() : r.blob();
}

// ---------- tooltips (mouse only, after 0.5 s, below the element) ----------
let tipTimer = 0;
document.addEventListener("pointerover", (e) => {
  const n = e.target.closest && e.target.closest("[data-tip]");
  clearTimeout(tipTimer);
  $("tip").classList.add("hidden");
  if (!n || e.pointerType !== "mouse") return;
  tipTimer = setTimeout(() => {
    const r = n.getBoundingClientRect();
    const tip = $("tip");
    tip.textContent = t(n.dataset.tip);
    tip.classList.remove("hidden");
    const w = tip.offsetWidth;
    tip.style.left = Math.max(4, Math.min(window.innerWidth - w - 4, r.left + r.width / 2 - w / 2)) + "px";
    tip.style.top = (r.bottom + 6) + "px";
  }, 500);
});
document.addEventListener("pointerdown", () => { clearTimeout(tipTimer); $("tip").classList.add("hidden"); });

// ---------- pairing ----------
function guessName() {
  const ua = navigator.userAgent;
  if (/iPad|Macintosh/.test(ua) && navigator.maxTouchPoints > 1) return "iPad";
  if (/iPhone/.test(ua)) return "iPhone";
  if (/Android/.test(ua)) return "Android";
  if (/Windows/.test(ua)) return "Windows";
  if (/Macintosh/.test(ua)) return "Mac";
  if (/Linux/.test(ua)) return "Linux";
  return "Browser";
}
function showPair() {
  $("app").classList.add("hidden");
  $("loupe").classList.add("hidden");
  $("pair").classList.remove("hidden");
  if (!$("pair-name").value) $("pair-name").value = guessName();
}
async function submitPair(ev) {
  if (ev) ev.preventDefault();
  $("pair-error").textContent = "";
  try {
    const r = await api("POST", "/api/pair", { code: $("pair-code").value, name: $("pair-name").value });
    state.csrf = r.csrf;
    $("pair-code").value = "";
    $("pair").classList.add("hidden");
    await startApp();
  } catch (e) {
    const m = { wrong_code: "pair_wrong", no_code: "pair_no_code", blocked: "pair_blocked", too_many_devices: "pair_too_many" }[e.code];
    $("pair-error").textContent = t(m || "error");
  }
}

// ---------- library ----------
function setSourceName(name) { $("src-name").textContent = name; }
async function loadSources() {
  const s = await api("GET", "/api/sources");
  const d = $("sources");
  d.replaceChildren();
  const pick = (src, name) => () => {
    state.source = src; store("source", src); store("sourceName", name);
    setSourceName(name); d.classList.add("hidden"); reload();
  };
  const btn = (src, name, n, depth) => {
    const b = el("button", { class: state.source === src ? "on" : "", onclick: pick(src, name) }, el("span", { text: name }), el("span", { class: "n", text: n == null ? "" : String(n) }));
    if (depth) b.style.paddingLeft = (14 + depth * 14) + "px";
    return b;
  };
  d.append(btn("all", t("all_photos"), s.total, 0));
  if (s.collections.length) {
    d.append(el("h2", { text: t("collections") }));
    const kids = {};
    for (const c of s.collections) (kids[c.parent || 0] = kids[c.parent || 0] || []).push(c);
    const walk = (parent, depth) => {
      for (const c of kids[parent] || []) {
        if (c.set) { d.append(el("h2", { text: c.name })); walk(c.id, depth); continue; }
        d.append(btn("collection:" + c.id, c.name, c.smart ? null : c.count, depth));
        walk(c.id, depth + 1);
      }
    };
    walk(0, 0);
  }
  for (const r of s.roots) {
    if (!r.folders.length) continue;
    d.append(el("h2", { text: r.name }));
    for (const f of r.folders) d.append(btn("folder:" + f.id, f.name, f.count || null, f.depth));
  }
}

function query(offset) {
  const p = new URLSearchParams({ src: state.source, sort: state.sort, offset: String(offset), limit: "200" });
  if (state.q) p.set("q", state.q);
  if (state.minStars) p.set("min", String(state.minStars));
  if (state.flag) p.set("flag", state.flag);
  if (state.label) p.set("label", String(state.label));
  return "/api/images?" + p.toString();
}

async function reload() {
  state.gen++;
  state.items = []; state.total = 0; state.sel = -1;
  $("grid").replaceChildren();
  window.scrollTo(0, 0);
  await more();
}

async function more() {
  if (state.loading || (state.items.length && state.items.length >= state.total)) return;
  state.loading = true;
  const gen = state.gen;
  try {
    const r = await api("GET", query(state.items.length));
    if (gen !== state.gen) return;
    state.total = r.total;
    const frag = document.createDocumentFragment();
    for (const it of r.items) { frag.append(tile(it, state.items.length)); state.items.push(it); }
    $("grid").append(frag);
    $("count").textContent = r.total + " " + t("photos");
    $("empty").classList.toggle("hidden", r.total > 0);
  } catch (e) {
    if (e.status !== 401) toast(t("error"));
  } finally {
    state.loading = false;
  }
  if (document.body.scrollHeight <= window.innerHeight * 1.5 && state.items.length < state.total) more();
}

function starsText(n) { return n ? "★".repeat(n) : ""; }

function thumbUrl(it) { return "/api/image/" + it.id + "/thumb" + (it.v ? "?v=" + it.v : ""); }

function loadImg(img, url, tries) {
  img.onload = () => img.classList.add("ok");
  img.onerror = () => {
    // 503 while graineo makes a missing preview: try again a little later
    if ((tries || 0) < 6) setTimeout(() => loadImg(img, url + (url.includes("?") ? "&" : "?") + "r=" + Date.now(), (tries || 0) + 1), 800 * ((tries || 0) + 1));
  };
  img.src = url;
}

const seen = new IntersectionObserver((entries) => {
  for (const e of entries) {
    if (!e.isIntersecting) continue;
    const img = e.target.querySelector("img");
    if (img && !img.src) loadImg(img, thumbUrl(state.items[Number(e.target.dataset.i)]));
    seen.unobserve(e.target);
  }
}, { rootMargin: "600px" });

function tile(it, i) {
  const meta = el("div", { class: "meta" },
    el("span", { class: "st", text: starsText(it.stars) }),
    it.label ? el("span", { class: "dot c" + it.label }) : null,
    el("span", { class: "fl", text: it.flag === 1 ? "⚑" : it.flag === -1 ? "✕" : "" }));
  const n = el("div", { class: "tile" + (it.flag === -1 ? " rejected" : ""), dataset: { i: String(i) }, onclick: () => openLoupe(i) }, el("img", { alt: "", decoding: "async" }), meta);
  seen.observe(n);
  return n;
}

function refreshTile(i) {
  const n = $("grid").children[i];
  if (!n) return;
  const it = state.items[i];
  const fresh = tile(it, i);
  const img = fresh.querySelector("img");
  loadImg(img, thumbUrl(it));
  seen.unobserve(fresh);
  if (i === state.sel) fresh.classList.add("sel");
  n.replaceWith(fresh);
}

function select(i) {
  const g = $("grid");
  if (state.sel >= 0 && g.children[state.sel]) g.children[state.sel].classList.remove("sel");
  state.sel = Math.max(0, Math.min(state.items.length - 1, i));
  const n = g.children[state.sel];
  if (n) { n.classList.add("sel"); n.scrollIntoView({ block: "nearest" }); }
  if (state.sel > state.items.length - 30) more();
}

// ---------- filters ----------
function seg(id, options, current, onpick) {
  const box = $(id);
  box.replaceChildren();
  for (const [value, label] of options) {
    box.append(el("button", { class: value === current ? "on" : "", onclick: () => { onpick(value); buildFilters(); reload(); } }, label));
  }
}
function buildFilters() {
  seg("f-stars", [[0, t("any")], [1, "★"], [2, "★★"], [3, "★★★"], [4, "★★★★"], [5, "★★★★★"]], state.minStars, (v) => { state.minStars = v; });
  seg("f-flag", [["", t("any")], ["picked", t("flag_picked")], ["unrejected", t("flag_unrejected")], ["rejected", t("flag_rejected")]], state.flag, (v) => { state.flag = v; });
  const labels = [[0, t("any")]];
  for (let i = 1; i <= 5; i++) labels.push([i, el("span", { class: "dot c" + i, title: t("label_" + i) })]);
  seg("f-label", labels, state.label, (v) => { state.label = v; });
  seg("f-sort", [["date_desc", t("sort_newest")], ["date_asc", t("sort_oldest")], ["name", t("sort_name")], ["stars", t("sort_stars")]], state.sort, (v) => { state.sort = v; });
  const n = (state.minStars ? 1 : 0) + (state.flag ? 1 : 0) + (state.label ? 1 : 0);
  $("filter-count").textContent = n ? String(n) : "";
  store("filters", { minStars: state.minStars, flag: state.flag, label: state.label, sort: state.sort });
}

// ---------- rating ----------
async function rate(i, change) {
  const it = state.items[i];
  if (!it) return;
  if (state.device && state.device.role !== "write") { toast(t("read_only")); return; }
  const old = { stars: it.stars, flag: it.flag, label: it.label };
  Object.assign(it, change);
  refreshTileMeta(i);
  if (state.loupe === i) showLoupeMeta();
  try {
    const r = await api("POST", "/api/image/" + it.id + "/rating", change);
    Object.assign(it, r);
  } catch (e) {
    Object.assign(it, old);
    toast(e.code === "read_only" ? t("read_only") : t("not_saved"));
  }
  refreshTileMeta(i);
  if (state.loupe === i) showLoupeMeta();
}
function refreshTileMeta(i) {
  const n = $("grid").children[i];
  const it = state.items[i];
  if (!n || !it) return;
  const meta = n.querySelector(".meta");
  meta.replaceChildren(
    el("span", { class: "st", text: starsText(it.stars) }),
    it.label ? el("span", { class: "dot c" + it.label }) : "",
    el("span", { class: "fl", text: it.flag === 1 ? "⚑" : it.flag === -1 ? "✕" : "" }));
  n.classList.toggle("rejected", it.flag === -1);
}
function keyRating(i, e) {
  const k = e.key.toLowerCase();
  if (k >= "0" && k <= "5") return rate(i, { stars: Number(k) }), true;
  if (k >= "6" && k <= "9") { const l = Number(k) - 5; return rate(i, { label: state.items[i].label === l ? 0 : l }), true; }
  if (k === "p") return rate(i, { flag: 1 }), true;
  if (k === "x") return rate(i, { flag: -1 }), true;
  if (k === "u") return rate(i, { flag: 0 }), true;
  return false;
}

// ---------- single photo ----------
const view = { s: 1, x: 0, y: 0 };
function applyView() { $("photo").style.transform = "translate(" + view.x + "px," + view.y + "px) scale(" + view.s + ")"; }
function resetView() { view.s = 1; view.x = 0; view.y = 0; applyView(); }

function setPhoto(url) {
  if (state.photoUrl.startsWith("blob:")) URL.revokeObjectURL(state.photoUrl);
  state.photoUrl = url;
  $("photo").src = url;
}

function openLoupe(i) {
  if (i < 0 || i >= state.items.length) return;
  if (state.dev && state.dev.dirty && !confirm(t("unsaved"))) return;
  closeDevelop(true);
  state.loupe = i;
  select(i);
  const it = state.items[i];
  $("loupe").classList.remove("hidden");
  $("loupe").focus();
  resetView();
  setPhoto(thumbUrl(it));
  const big = new Image();
  big.onload = () => { if (state.loupe === i && !state.dev) setPhoto(big.src); };
  big.src = "/api/image/" + it.id + "/large" + (it.v ? "?v=" + it.v : "");
  // the neighbours, so swiping is quick
  for (const j of [i + 1, i - 1]) { const n = state.items[j]; if (n) { const p = new Image(); p.src = "/api/image/" + n.id + "/large" + (n.v ? "?v=" + n.v : ""); } }
  $("loupe-name").textContent = it.name + (it.copy ? " · " + it.copy : "");
  $("dev-btn").classList.toggle("hidden", !(state.device && state.device.role === "write") || it.video);
  showLoupeMeta();
  if (!$("info").classList.contains("hidden")) showInfo();
  if (i > state.items.length - 20) more();
}
function closeLoupe() {
  if (state.dev && state.dev.dirty && !confirm(t("unsaved"))) return;
  closeDevelop(true);
  $("loupe").classList.add("hidden");
  state.loupe = -1;
  setPhoto("");
  $("grid").focus();
}
function step(d) { openLoupe(state.loupe + d); }

function showLoupeMeta() {
  const it = state.items[state.loupe];
  if (!it) return;
  const st = $("l-stars");
  st.replaceChildren();
  for (let n = 1; n <= 5; n++) {
    st.append(el("button", { class: n <= it.stars ? "on" : "", "aria-label": t("stars_n", { n }), onclick: () => rate(state.loupe, { stars: it.stars === n ? 0 : n }) }, "★"));
  }
  $("l-pick").classList.toggle("on", it.flag === 1);
  $("l-reject").classList.toggle("on", it.flag === -1);
  const lb = $("l-labels");
  lb.replaceChildren();
  for (let n = 1; n <= 5; n++) {
    lb.append(el("button", { class: "c" + n + (it.label === n ? " on" : ""), "aria-label": t("label_" + n), onclick: () => rate(state.loupe, { label: it.label === n ? 0 : n }) }));
  }
}

async function showInfo() {
  const it = state.items[state.loupe];
  const box = $("info");
  box.classList.remove("hidden");
  $("dev").classList.add("hidden");
  box.replaceChildren(el("p", { class: "muted", text: t("loading") }));
  try {
    const d = await api("GET", "/api/image/" + it.id);
    const dl = el("dl");
    const add = (k, v) => { if (v) dl.append(el("dt", { text: t(k) }), el("dd", { text: v })); };
    add("file", d.name + (d.copy ? " · " + t("copy") + " " + d.copy : ""));
    add("date", d.date ? d.date.replace("T", " ") : "");
    add("camera", d.camera);
    add("lens", d.lens);
    const ex = [d.time ? (d.time >= 1 ? d.time + " s" : "1/" + Math.round(1 / d.time) + " s") : "", d.aperture ? "f/" + d.aperture : "", d.iso ? "ISO " + d.iso : "", d.focal ? Math.round(d.focal) + " mm" : ""].filter(Boolean).join(" · ");
    add("exposure_info", ex);
    add("place", d.place);
    add("keywords", (d.keywords || []).join(", "));
    add("collections", (d.collections || []).join(", "));
    box.replaceChildren(dl);
  } catch (_) {
    box.replaceChildren(el("p", { class: "error", text: t("error") }));
  }
}

// ---------- develop ----------
const GROUPS = [["white_balance", ["temp", "tint"]], ["tone", ["exposure", "contrast", "highlights", "shadows", "whites", "blacks"]], ["presence", ["clarity", "vibrance", "saturation"]]];

function fmt(key, v) {
  if (key === "exposure") return (v > 0 ? "+" : "") + v.toFixed(2);
  if (key === "temp" && state.dev && state.dev.kind === "raw") return Math.round(v) + " K";
  const step = state.dev ? (state.dev.sliders.find((s) => s.key === key) || {}).step : 1;
  return (v > 0 ? "+" : "") + (step < 1 ? v.toFixed(2) : String(Math.round(v)));
}

async function openDevelop() {
  const it = state.items[state.loupe];
  if (!it) return;
  if (state.dev) { closeDevelop(); return; }
  $("info").classList.add("hidden");
  const box = $("dev");
  box.classList.remove("hidden");
  $("dev-sliders").replaceChildren(el("p", { class: "muted", text: t("loading") }));
  $("dev-note").classList.add("hidden");
  try {
    const d = await api("GET", "/api/image/" + it.id + "/develop");
    state.dev = { id: it.id, index: state.loupe, kind: d.kind, sliders: d.sliders, wbModes: d.wb_modes, loaded: Object.assign({}, d.values), values: Object.assign({}, d.values), version: d.version, dirty: false };
    buildSliders();
    if (d.open_in_app) note(t("open_in_app"));
    $("dev-save").disabled = true;
  } catch (e) {
    state.dev = null;
    const m = { original_unreachable: "original_unreachable", not_developable: "not_developable", read_only: "read_only" }[e.code];
    $("dev-sliders").replaceChildren(el("p", { class: "error", text: t(m || "error") }));
  }
}
function note(text) { const n = $("dev-note"); n.textContent = text; n.classList.remove("hidden"); }

function buildSliders() {
  const dv = state.dev;
  const box = $("dev-sliders");
  box.replaceChildren();
  for (const [group, keys] of GROUPS) {
    const present = keys.filter((k) => dv.sliders.some((s) => s.key === k));
    if (!present.length) continue;
    box.append(el("div", { class: "group", text: t(group) }));
    if (group === "white_balance" && dv.wbModes.length) {
      const sel = el("select", { onchange: (e) => { dv.values.wb_mode = e.target.value; changed(); } });
      for (const m of dv.wbModes) sel.append(el("option", { value: m, text: t("wb_" + m) }));
      sel.value = dv.values.wb_mode || "as_shot";
      box.append(sel);
    }
    for (const k of present) {
      const s = dv.sliders.find((x) => x.key === k);
      const out = el("output", { text: fmt(k, dv.values[k]) });
      const row = el("div", { class: "sl", dataset: { k } });
      const input = el("input", { type: "range", min: String(s.min), max: String(s.max), step: String(s.step), value: String(dv.values[k]) });
      input.addEventListener("input", () => { dv.values[k] = Number(input.value); out.textContent = fmt(k, dv.values[k]); changed(); });
      // double tap / double click on a slider: back to the loaded value
      input.addEventListener("dblclick", () => { dv.values[k] = dv.loaded[k]; input.value = String(dv.loaded[k]); out.textContent = fmt(k, dv.loaded[k]); changed(); });
      row.append(el("span", { text: t(k) }), out, input);
      box.append(row);
    }
  }
  const mono = el("input", { type: "checkbox" });
  mono.checked = !!dv.values.monochrome;
  mono.addEventListener("change", () => { dv.values.monochrome = mono.checked; changed(); });
  box.append(el("label", { class: "check" }, mono, el("span", { text: t("monochrome") })));
}

function diff() {
  const dv = state.dev;
  const out = {};
  for (const k in dv.values) if (dv.values[k] !== dv.loaded[k]) out[k] = dv.values[k];
  return out;
}

function changed() {
  const dv = state.dev;
  const d = diff();
  dv.dirty = Object.keys(d).length > 0;
  $("dev-save").disabled = !dv.dirty;
  $("dev-state").textContent = dv.dirty ? t("not_saved") : "";
  document.querySelectorAll("#dev-sliders .sl").forEach((r) => r.classList.toggle("changed", r.dataset.k in d));
  clearTimeout(state.renderTimer);
  state.renderTimer = setTimeout(render, 250);
}

async function render() {
  const dv = state.dev;
  if (!dv) return;
  if (state.renderBusy) { state.renderAgain = true; return; }
  state.renderBusy = true;
  $("spinner").classList.remove("hidden");
  try {
    const blob = await api("POST", "/api/image/" + dv.id + "/render", { values: diff() });
    if (state.dev === dv) setPhoto(URL.createObjectURL(blob));
  } catch (e) {
    if (e.status === 503) { state.renderAgain = true; await new Promise((r) => setTimeout(r, 600)); }
    else if (e.status !== 401) toast(t(e.code === "original_unreachable" ? "original_unreachable" : "error"));
  } finally {
    state.renderBusy = false;
    $("spinner").classList.add("hidden");
  }
  if (state.renderAgain) { state.renderAgain = false; render(); }
}

async function saveDevelop() {
  const dv = state.dev;
  if (!dv || !dv.dirty) return;
  $("dev-save").disabled = true;
  try {
    const r = await api("POST", "/api/image/" + dv.id + "/develop", { values: diff(), version: dv.version });
    dv.version = r.version;
    dv.loaded = Object.assign({}, dv.values);
    dv.dirty = false;
    changed();
    clearTimeout(state.renderTimer);
    toast(t("saved"), 1800);
    const it = state.items[dv.index];
    if (it) { it.v = Date.now(); refreshTile(dv.index); }
  } catch (e) {
    $("dev-save").disabled = false;
    if (e.code === "changed_elsewhere") {
      toast(t("changed_elsewhere"), 6000);
      state.dev = null;
      openDevelop();
    } else if (e.code === "open_in_app") {
      note(t("open_in_app"));
    } else if (e.status !== 401) {
      toast(t("not_saved") + " – " + t(e.code === "original_unreachable" ? "original_unreachable" : "error"), 6000);
    }
  }
}

function resetDevelop() {
  const dv = state.dev;
  if (!dv) return;
  dv.values = Object.assign({}, dv.loaded);
  buildSliders();
  changed();
}

function closeDevelop(silent) {
  const dv = state.dev;
  if (!silent && dv && dv.dirty && !confirm(t("unsaved"))) return;
  state.dev = null;
  clearTimeout(state.renderTimer);
  $("dev").classList.add("hidden");
  $("spinner").classList.add("hidden");
  if (dv && state.loupe >= 0 && !silent) {
    const it = state.items[state.loupe];
    setPhoto("/api/image/" + it.id + "/large" + (it.v ? "?v=" + it.v : ""));
  }
}

// ---------- gestures in the single view ----------
function gestures() {
  const stage = $("stage");
  const pts = new Map();
  let start = null;
  let lastTap = 0;
  stage.addEventListener("pointerdown", (e) => {
    stage.setPointerCapture(e.pointerId);
    pts.set(e.pointerId, { x: e.clientX, y: e.clientY });
    start = { pts: new Map([...pts].map(([k, v]) => [k, Object.assign({}, v)])), view: Object.assign({}, view), t: Date.now() };
  });
  stage.addEventListener("pointermove", (e) => {
    if (!pts.has(e.pointerId) || !start) return;
    pts.set(e.pointerId, { x: e.clientX, y: e.clientY });
    const a = [...pts.values()], b = [...start.pts.values()];
    if (a.length >= 2 && b.length >= 2) {
      const d0 = Math.hypot(b[0].x - b[1].x, b[0].y - b[1].y) || 1;
      const d1 = Math.hypot(a[0].x - a[1].x, a[0].y - a[1].y);
      view.s = Math.max(1, Math.min(6, start.view.s * d1 / d0));
      const cx0 = (b[0].x + b[1].x) / 2, cy0 = (b[0].y + b[1].y) / 2, cx1 = (a[0].x + a[1].x) / 2, cy1 = (a[0].y + a[1].y) / 2;
      view.x = start.view.x + (cx1 - cx0); view.y = start.view.y + (cy1 - cy0);
      applyView();
    } else if (view.s > 1 && a.length === 1) {
      const p = start.pts.get(e.pointerId);
      if (p) { view.x = start.view.x + (e.clientX - p.x); view.y = start.view.y + (e.clientY - p.y); applyView(); }
    }
  });
  const end = (e) => {
    if (!pts.has(e.pointerId)) return;
    const p0 = start && start.pts.get(e.pointerId);
    const multi = start && start.pts.size > 1;
    pts.delete(e.pointerId);
    if (!p0 || multi) { if (view.s <= 1.02) resetView(); start = pts.size ? { pts: new Map([...pts]), view: Object.assign({}, view), t: Date.now() } : null; return; }
    const dx = e.clientX - p0.x, dy = e.clientY - p0.y, dt = Date.now() - start.t;
    start = null;
    if (view.s <= 1.02 && Math.abs(dx) > 60 && Math.abs(dx) > 1.5 * Math.abs(dy) && dt < 800) { step(dx < 0 ? 1 : -1); return; }
    if (Math.abs(dx) < 10 && Math.abs(dy) < 10 && dt < 300) {
      const now = Date.now();
      if (now - lastTap < 320) { if (view.s > 1) resetView(); else { view.s = 2.5; view.x = 0; view.y = 0; applyView(); } lastTap = 0; }
      else lastTap = now;
    }
  };
  stage.addEventListener("pointerup", end);
  stage.addEventListener("pointercancel", end);
  stage.addEventListener("wheel", (e) => {
    e.preventDefault();
    view.s = Math.max(1, Math.min(6, view.s * (e.deltaY < 0 ? 1.1 : 1 / 1.1)));
    if (view.s === 1) { view.x = 0; view.y = 0; }
    applyView();
  }, { passive: false });
}

// ---------- keyboard ----------
document.addEventListener("keydown", (e) => {
  if (e.target && (e.target.tagName === "INPUT" && e.target.type !== "range" && e.target.type !== "checkbox" || e.target.tagName === "SELECT")) return;
  if (e.metaKey || e.ctrlKey || e.altKey) return;
  const inLoupe = state.loupe >= 0;
  if (inLoupe) {
    if (e.key === "Escape") { if (state.dev) closeDevelop(); else closeLoupe(); e.preventDefault(); return; }
    if (e.target && e.target.type === "range") return;
    if (e.key === "ArrowRight") { step(1); e.preventDefault(); return; }
    if (e.key === "ArrowLeft") { step(-1); e.preventDefault(); return; }
    if (e.key === "i") { if ($("info").classList.contains("hidden")) showInfo(); else $("info").classList.add("hidden"); return; }
    if (e.key === "d" && state.device && state.device.role === "write") { openDevelop(); return; }
    if (keyRating(state.loupe, e)) e.preventDefault();
    return;
  }
  if (!state.items.length) return;
  const cols = Math.max(1, Math.round($("grid").clientWidth / (($("grid").children[0] || {}).offsetWidth || 150)));
  const moves = { ArrowRight: 1, ArrowLeft: -1, ArrowDown: cols, ArrowUp: -cols };
  if (moves[e.key] != null) { select(state.sel < 0 ? 0 : state.sel + moves[e.key]); e.preventDefault(); return; }
  if (e.key === "Enter" && state.sel >= 0) { openLoupe(state.sel); return; }
  if (state.sel >= 0 && keyRating(state.sel, e)) e.preventDefault();
});

// ---------- start ----------
async function startApp() {
  try {
    const s = await api("GET", "/api/session");
    state.csrf = s.csrf;
    state.device = s.device;
  } catch (_) { return; }
  $("pair").classList.add("hidden");
  $("app").classList.remove("hidden");
  $("device-name").textContent = state.device.name;
  $("device-role").textContent = state.device.role === "write" ? "" : t("read_only");
  setSourceName(recall("sourceName", t("all_photos")));
  buildFilters();
  try { await loadSources(); } catch (_) {}
  await reload();
}

function sideState() {
  const open = !$("info").classList.contains("hidden") || !$("dev").classList.contains("hidden");
  $("loupe").classList.toggle("with-side", open);
}

function wire() {
  const mo = new MutationObserver(sideState);
  mo.observe($("info"), { attributes: true, attributeFilter: ["class"] });
  mo.observe($("dev"), { attributes: true, attributeFilter: ["class"] });
  $("pair-form").addEventListener("submit", submitPair);
  $("src-btn").addEventListener("click", () => { $("sources").classList.toggle("hidden"); $("filters").classList.add("hidden"); $("menu").classList.add("hidden"); });
  $("filter-btn").addEventListener("click", () => { $("filters").classList.toggle("hidden"); $("sources").classList.add("hidden"); $("menu").classList.add("hidden"); });
  $("menu-btn").addEventListener("click", () => { $("menu").classList.toggle("hidden"); $("sources").classList.add("hidden"); $("filters").classList.add("hidden"); });
  $("grid").addEventListener("click", () => $("sources").classList.add("hidden"));
  let searchTimer = 0;
  $("search").addEventListener("input", () => { clearTimeout(searchTimer); searchTimer = setTimeout(() => { state.q = $("search").value.trim(); reload(); }, 350); });
  $("logout").addEventListener("click", async () => {
    if (!confirm(t("logout_confirm"))) return;
    try { await api("POST", "/api/logout"); } catch (_) {}
    showPair();
  });
  $("close").addEventListener("click", closeLoupe);
  $("prev").addEventListener("click", () => step(-1));
  $("next").addEventListener("click", () => step(1));
  $("info-btn").addEventListener("click", () => { if ($("info").classList.contains("hidden")) showInfo(); else $("info").classList.add("hidden"); });
  $("dev-btn").addEventListener("click", openDevelop);
  $("l-pick").addEventListener("click", () => { const it = state.items[state.loupe]; rate(state.loupe, { flag: it.flag === 1 ? 0 : 1 }); });
  $("l-reject").addEventListener("click", () => { const it = state.items[state.loupe]; rate(state.loupe, { flag: it.flag === -1 ? 0 : -1 }); });
  $("dev-save").addEventListener("click", saveDevelop);
  $("dev-reset").addEventListener("click", resetDevelop);
  window.addEventListener("scroll", () => { if (window.innerHeight + window.scrollY > document.body.scrollHeight - 1200) more(); }, { passive: true });
  window.addEventListener("beforeunload", (e) => { if (state.dev && state.dev.dirty) { e.preventDefault(); e.returnValue = ""; } });
  gestures();
}

// pairing link from the QR code: the code is in the fragment (never sent to the server), removed at once
async function pairFromLink() {
  const m = /^#pair=([A-Za-z0-9-]{8,9})$/.exec(location.hash);
  if (!m) return false;
  history.replaceState(null, "", location.pathname);
  try {
    const s = await fetch("/api/session", { credentials: "same-origin" });
    if (s.ok) { await startApp(); return true; }
  } catch (_) {}
  showPair();
  $("pair-code").value = m[1].toUpperCase();
  await submitPair();
  return true;
}

(async function main() {
  await loadTexts();
  wire();
  const f = recall("filters", null);
  if (f) Object.assign(state, { minStars: f.minStars || 0, flag: f.flag || "", label: f.label || 0, sort: f.sort || "date_desc" });
  state.source = recall("source", "all");
  window.addEventListener("hashchange", pairFromLink);
  if (await pairFromLink()) return;
  const s = await fetch("/api/session", { credentials: "same-origin" }).catch(() => null);
  if (!s) { showPair(); toast(t("offline"), 8000); return; }
  if (s.status === 401) { showPair(); return; }
  await startApp();
})();
