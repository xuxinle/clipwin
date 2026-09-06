import { useEffect, useRef, useState, useCallback } from "react";
import { invoke, convertFileSrc } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useVirtualizer } from "@tanstack/react-virtual";
import TodoEditor, { type TodoEditDraft } from "./TodoEditor";
import CalendarPopover from "./CalendarPopover";
import "./App.css";

interface ClipRow {
  id: number;
  ts: number;
  kind: string;
  text: string | null;
  image_path: string | null;
  files_json: string | null;
  html_path: string | null;
  hash: string;
  size: number;
  pinned: boolean;
}

interface TodoRow {
  id: number;
  ts: number;
  done: boolean;
  kind: string;
  title: string | null;
  content: string | null;
  text: string | null;
  image_path: string | null;
  files_json: string | null;
  size: number;
  priority: number;
  due_at: number | null;
  tags: string;
  pinned: boolean;
  done_at: number | null;
}

interface TodoStats {
  open: number;
  today: number;
  overdue: number;
  done: number;
  tags: Record<string, number>;
}

const KIND_ICON: Record<string, string> = { text: "📝", image: "🖼️", files: "📁" };

/** 相对时间：刚刚/N 分钟前/N 小时前/昨天/M-D */
function relTime(ts: number): string {
  const diff = Date.now() - ts;
  if (diff < 60_000) return "刚刚";
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)} 分钟前`;
  if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)} 小时前`;
  if (diff < 172_800_000) return "昨天";
  const d = new Date(ts);
  return `${d.getMonth() + 1}/${d.getDate()}`;
}
const PRIO_ICON: Record<number, string> = { 0: "🏳️", 1: "🟢", 2: "🟡", 3: "🔴" };
const PRIO_NEXT: Record<number, number> = { 0: 3, 3: 1, 1: 2, 2: 0 }; // 点击循环：无→高→低→中→无
const PRIO_NAME: Record<number, string> = { 0: "无优先级", 1: "低", 2: "中", 3: "高" };

/** 解析快捷输入语法：#标签、!1/!2/!3 优先级（可多个 #） */
function parseQuick(raw: string): { text: string; tags: string[]; priority: number | null } {
  let text = raw;
  const tags: string[] = [];
  let priority: number | null = null;
  text = text.replace(/#([^\s#!]+)/g, (_, t) => { tags.push(t); return ""; });
  text = text.replace(/!([123])\b/g, (_, p) => { if (priority === null) priority = Number(p); return ""; });
  return { text: text.replace(/\s+/g, " ").trim(), tags, priority };
}

/** 富文本 → 纯文本（保存待办时写 text 字段：搜索/回贴用） */
function htmlToText(html: string): string {
  const d = document.createElement("div");
  d.innerHTML = html;
  return (d.innerText || d.textContent || "").trim();
}

function safeJson(s: string | null | undefined): string[] {
  try { return s ? JSON.parse(s) : []; } catch { return []; }
}

/** 截止日显示：逾期红 / 今天橙 / 明天蓝 / 其他灰 */
function dueLabel(due: number): { label: string; cls: string } {
  const now = new Date();
  const today0 = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
  const d = new Date(due);
  const due0 = new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  const diff = Math.round((due0 - today0) / 86400000);
  if (diff < 0) return { label: `逾期 ${-diff} 天`, cls: "due overdue" };
  if (diff === 0) return { label: "今天", cls: "due today" };
  if (diff === 1) return { label: "明天", cls: "due soon" };
  return { label: `${d.getMonth() + 1}/${d.getDate()}`, cls: "due" };
}

/** 开机自启开关（autostart 插件 API） */
function AutoStartToggle() {
  const [on, setOn] = useState<boolean | null>(null);
  useEffect(() => {
    (async () => {
      try {
        const { isEnabled } = await import("@tauri-apps/plugin-autostart");
        setOn(await isEnabled());
      } catch { setOn(false); }
    })();
  }, []);
  return (
    <button
      className={on ? "seg-btn active" : "seg-btn"}
      onClick={async () => {
        try {
          const { isEnabled, enable, disable } = await import("@tauri-apps/plugin-autostart");
          if (await isEnabled()) { await disable(); setOn(false); }
          else { await enable(); setOn(true); }
        } catch (e) { console.error(e); }
      }}
    >{on === null ? "读取中…" : on ? "已开启（点击关闭）" : "已关闭（点击开启）"}</button>
  );
}
/** 右键菜单状态 */
interface CtxMenu { x: number; y: number; clipId: number; }

const TXT_LEN = 120; // 超过此长度截断 + 展开按钮

export default function App() {
  // ---- 视图状态 ----
  const [tab, setTab] = useState<"clips" | "todos" | "settings">("clips");
  const [ctxMenu, setCtxMenu] = useState<CtxMenu | null>(null);

  // ---- 剪贴板列表状态（M1 原有） ----
  const [rows, setRows] = useState<ClipRow[]>([]);
  const [query, setQuery] = useState("");
  const [selId, setSelId] = useState<number | null>(null);
  const [stats, setStats] = useState<{ count: number }>({ count: 0 });
  const [bench, setBench] = useState(""); // 仅作错误提示条复用（压测已移除）
  const parentRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const queryTimer = useRef<number | undefined>(undefined);
  const queryRef = useRef("");
  const rowsRef = useRef<ClipRow[]>([]);
  const selIdRef = useRef<number | null>(null);

  queryRef.current = query;
  rowsRef.current = rows;
  selIdRef.current = selId;

  // ---- 待办状态（M3 增强） ----
  const [todos, setTodos] = useState<TodoRow[]>([]);
  const [todoStats, setTodoStats] = useState<TodoStats | null>(null);
  const [todoInput, setTodoInput] = useState("");
  const [filter, setFilter] = useState<"open" | "today" | "overdue" | "done" | "all">("open");
  const [tagFilter, setTagFilter] = useState("");
  const [editor, setEditor] = useState<TodoEditDraft | null>(null); // 待办编辑弹窗（null=关）
  const [dueFor, setDueFor] = useState<number | null>(null); // 列表行内日历（快捷改日期）
  const [dueAnchor, setDueAnchor] = useState<DOMRect | null>(null); // 日历锚点按钮屏幕坐标
  const todoInputRef = useRef<HTMLInputElement>(null);

  const [viewerImg, setViewerImg] = useState<string | null>(null); // 图片放大查看器
  const [expanded, setExpanded] = useState<number | null>(null); // 长文本展开的条目 id
  // ---- 设置 ----
  const [settings, setSettings] = useState<{ hotkey: string; hotkey_todos: string; history_limit: number; paused: boolean } | null>(null);
  const [hkInput, setHkInput] = useState("");
  const [hkRecording, setHkRecording] = useState(false);
  const [hkTInput, setHkTInput] = useState("");
  const [hkTRecording, setHkTRecording] = useState(false);

  async function refresh(q: string) {
    try {
      const r = await invoke<ClipRow[]>("search", { query: q, limit: 10000 });
      setRows(r);
      setSelId((prev) => (prev != null && r.some((x) => x.id === prev) ? prev : r[0]?.id ?? null));
        const s = await invoke<{ count: number }>("stats");
      setStats(s);
    } catch (e) {
      setBench(`错误：${e}`);
    }
  }

  async function refreshTodos() {
    try {
      const r = await invoke<TodoRow[]>("todos_list", { filter, tag: tagFilter, includeDone: false });
      setTodos(r);
      const s = await invoke<TodoStats>("todos_stats");
      setTodoStats(s);
    } catch (e) {
      setBench(`待办读取失败：${e}`);
    }
  }

  // 唤出后聚焦：WebView2 窗口激活有时序竞争，多次重试直到焦点真正落到输入框
  function focusInput(retry = 0) {
    inputRef.current?.focus();
    inputRef.current?.select();
    if (retry < 4) {
      window.setTimeout(() => {
        if (document.activeElement === inputRef.current && document.hasFocus()) return;
        focusInput(retry + 1);
      }, 60);
    }
  }
  useEffect(() => {
    refresh("");
    const un1 = listen("clip:new", () => refresh(queryRef.current));
    // 唤出事件：payload.tab 指定落在哪个 Tab（Alt+V=clips / Alt+T=todos）
    const un2 = listen<{ tab: string }>("clipwin:popup", (ev) => {
      const t = ev.payload?.tab === "todos" ? "todos" : "clips";
      if (t !== tabRef.current) setTab(t);
      if (t === "clips") focusInput();
      else todoInputRef.current?.focus();
    });
    // 窗口从隐藏到可见（托盘/热键唤出）：重新聚焦
    const onVis = () => {
      if (document.visibilityState === "visible") {
        // 每次唤出重拉列表（回贴置顶/新增条目立即反映）
        refresh(queryRef.current);
        if (tabRef.current === "clips") focusInput();
        else todoInputRef.current?.focus();
      }
    };
    document.addEventListener("visibilitychange", onVis);
    return () => {
      un1.then((f) => f()); un2.then((f) => f());
      document.removeEventListener("visibilitychange", onVis);
    };
  }, []);

  const tabRef = useRef(tab);
  tabRef.current = tab;

  useEffect(() => {
    window.clearTimeout(queryTimer.current);
    queryTimer.current = window.setTimeout(() => refresh(query), 150);
    return () => window.clearTimeout(queryTimer.current);
  }, [query]);

  useEffect(() => { refreshTodos(); }, [filter, tagFilter]);

  // ---- 剪贴板条目操作 ----
  async function pasteBack(id: number) {
    try {
      await invoke("paste_back", { id });
    } catch (e) {
      setBench(`回贴失败：${e}`);
    }
  }

  /** 右键「查看」：图片=放大查看器，文本=行内展开/收起 */
  function ctxView(id: number) {
    const row = rows.find((r) => r.id === id);
    if (!row) return;
    if (row.kind === "image" && row.image_path) {
      setViewerImg(convertFileSrc(row.image_path.replace(/\.dib$/, ".png")));
    } else {
      setExpanded(expanded === id ? null : id);
    }
  }

  function moveSel(delta: number) {
    const rs = rowsRef.current;
    if (rs.length === 0) return;
    const cur = rs.findIndex((r) => r.id === selIdRef.current);
    let next: number;
    if (cur === -1) next = delta > 0 ? 0 : rs.length - 1;
    else next = Math.min(rs.length - 1, Math.max(0, cur + delta));
    if (next === cur) return;
    const id = rs[next].id;
    setSelId(id);
    virtualizer.scrollToIndex(next, { align: "auto" });
  }

  async function onKeyDown(e: React.KeyboardEvent) {
    // IME 组合期（未上屏）不响应导航/回贴/隐藏
    if (e.nativeEvent.isComposing) return;
    if (e.key === "ArrowDown") { e.preventDefault(); moveSel(1); }
    else if (e.key === "ArrowUp") { e.preventDefault(); moveSel(-1); }
    else if (e.key === "Enter") {
      e.preventDefault();
      if (selIdRef.current != null) await pasteBack(selIdRef.current);
    } else if (e.key === "Escape") {
      e.preventDefault();
      await invoke("hide_window"); // Rust 侧：保存位置后隐藏
    }
  }

  async function loadSettings() {
    const s = await invoke<{ hotkey: string; hotkey_todos: string; history_limit: number; paused: boolean }>("settings_all");
    setSettings(s);
    setHkInput(s.hotkey);
    setHkTInput(s.hotkey_todos);
  }

  async function clipTogglePin(id: number) {
    setCtxMenu(null);
    await invoke("clip_toggle_pin", { id });
    refresh(queryRef.current);
  }

  async function clipDelete(id: number) {
    setCtxMenu(null);
    await invoke("clip_delete", { id });
    refresh(queryRef.current);
  }

  async function clipsClear() {
    setCtxMenu(null);
    if (!window.confirm("清空全部历史（收藏条目保留）？")) return;
    await invoke("clips_clear");
    refresh("");
  }

  // ---- 待办操作 ----
  /** 新建待办（标题模式）：标题为输入框内容（支持 #标签 !1!2!3 快捷语法），创建后弹编辑器补内容 */
  async function addTodo() {
    const raw = todoInput.trim();
    if (!raw) return;
    const { text, tags, priority } = parseQuick(raw);
    if (!text) return;
    try {
      const row = await invoke<TodoRow>("todos_add", {
        input: {
          kind: "text", title: text, content: null, text,
          imagePath: null, filesJson: null, size: text.length,
          priority: priority ?? 0, dueAt: null,
          tags: JSON.stringify(tags),
        },
      });
      setTodoInput("");
      openEditor(row, true); // 新建即编辑：光标落在内容区
      refreshTodos();
    } catch (e) {
      setBench(`添加失败：${e}`);
    }
  }

  /** 打开编辑弹窗：row → draft；focusContent=新建场景直接聚焦内容区 */
  function openEditor(t: TodoRow, isNew = false) {
    let tags: string[] = [];
    try { tags = JSON.parse(t.tags || "[]"); } catch { /* ignore */ }
    setEditor({
      id: t.id,
      title: t.title ?? "",
      contentHtml: t.content ?? "",
      priority: t.priority,
      due_at: t.due_at,
      tags,
      imageSrc: t.kind === "image" && t.image_path ? convertFileSrc(t.image_path.replace(/\.dib$/, ".png")) : null,
      files: t.kind === "files" ? safeJson(t.files_json) : undefined,
      rawText: t.text,
      isNew,
    } as TodoEditDraft & { isNew: boolean });
  }

  /** 弹窗保存：标题 + 富文本 + 纯文本 + 优先级/日期/标签（后三者即时生效过）
   * 编辑器内可能改了所有字段——未保存的属性变更在保存时一并提交 */
  async function saveEditor() {
    if (!editor) return;
    const contentEl = document.querySelector(".ed-content") as HTMLDivElement | null;
    const html = contentEl?.innerHTML ?? editor.contentHtml;
    // 富文本 → 纯文本（text 字段：搜索/回贴用）
    const plain = htmlToText(html);
    try {
      await invoke("todos_update_full", {
        id: editor.id, title: editor.title || "待办",
        contentHtml: html || null,
        text: plain || editor.rawText || null,
      });
      // 属性字段（弹窗里可能改过）
      await invoke("todos_set_priority", { id: editor.id, priority: editor.priority });
      await invoke("todos_set_due", { id: editor.id, dueMs: editor.due_at });
      await invoke("todos_set_tags", { id: editor.id, tags: editor.tags.join(",") });
      setEditor(null);
      refreshTodos();
    } catch (e) {
      setBench(`保存失败：${e}`);
    }
  }

  async function pasteFromClipboard() {
    try {
      await invoke("todo_paste_from_clipboard");
      refreshTodos();
    } catch (e) {
      setBench(`粘贴失败：${e}`);
    }
  }

  async function toggleTodo(id: number) {
    await invoke("todos_toggle", { id });
    refreshTodos();
  }

  async function deleteTodo(id: number) {
    await invoke("todos_delete", { id });
    refreshTodos();
  }

  async function cyclePriority(id: number, cur: number) {
    await invoke("todos_set_priority", { id, priority: PRIO_NEXT[cur] ?? 0 });
    refreshTodos();
  }

  async function togglePin(id: number) {
    await invoke("todos_toggle_pin", { id });
    refreshTodos();
  }

  async function setDue(id: number, ymd: string | null) {
    const ms = ymd ? new Date(ymd + "T00:00:00").getTime() : null;
    await invoke("todos_set_due", { id, dueMs: ms });
    setDueFor(null);
    refreshTodos();
  }

  /** 剪贴板条目右键存待办后：切到待办 Tab 并打开编辑弹窗（标题默认取前段） */
  async function saveClipAsTodoAndEdit(clipId: number) {
    setCtxMenu(null);
    try {
      const row = await invoke<TodoRow>("clip_to_todo", { id: clipId });
      setTab("todos");
      refreshTodos();
      openEditor(row);
    } catch (e) {
      setBench(`存待办失败：${e}`);
    }
  }

  // 待办视图键盘：Enter 添加；Ctrl+V 从系统剪贴板收任意格式；Esc 隐藏（走 Rust 保存位置）
  async function onTodoKeyDown(e: React.KeyboardEvent) {
    if (e.nativeEvent.isComposing) return;
    if (e.key === "Enter") { e.preventDefault(); addTodo(); }
    else if (e.key === "Escape") { e.preventDefault(); await invoke("hide_window"); }
  }

  // 待办视图：窗口级 Ctrl+V（粘贴任意格式为待办）——仅输入框外生效；
  // 输入框聚焦时放行原生粘贴（用户可能想在框里粘长文本作为待办内容）
  useEffect(() => {
    if (tab !== "todos") return;
    const onKey = async (e: KeyboardEvent) => {
      if (e.isComposing) return;
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "v") {
        const el = e.target as HTMLElement | null;
        const inInput = !!el && (el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.isContentEditable);
        if (inInput) return; // 输入框内：原生粘贴
        e.preventDefault();
        await pasteFromClipboard();
      }
    };
    document.addEventListener("keydown", onKey, true);
    return () => document.removeEventListener("keydown", onKey, true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tab]);

  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => parentRef.current,
    estimateSize: () => 56,
    getItemKey: (i: number) => rows[i]?.id ?? i,
    overscan: 12,
  });
  const items = virtualizer.getVirtualItems();
  const total = virtualizer.getTotalSize();
  const selIdx = rows.findIndex((r) => r.id === selId);

  const closeCtxMenu = useCallback(() => setCtxMenu(null), []);

  const chips: { key: typeof filter; label: string; count: number }[] = [
    { key: "open", label: "进行中", count: todoStats?.open ?? 0 },
    { key: "today", label: "今日", count: todoStats?.today ?? 0 },
    { key: "overdue", label: "逾期", count: todoStats?.overdue ?? 0 },
    { key: "done", label: "已完成", count: todoStats?.done ?? 0 },
    { key: "all", label: "全部", count: (todoStats?.open ?? 0) + (todoStats?.done ?? 0) },
  ];

  return (
    <div className="app" onKeyDown={tab === "clips" ? onKeyDown : undefined}>
      {/* Tab 栏 */}
      <div className="tabs">
        <button className={tab === "clips" ? "tab active" : "tab"} onClick={() => setTab("clips")}>
          📋 剪贴板
        </button>
        <button className={tab === "todos" ? "tab active" : "tab"} onClick={() => { setTab("todos"); refreshTodos(); }}>
          ✅ 待办 {todoStats && todoStats.open > 0 && <span className="badge">{todoStats.open}</span>}
        </button>
        <button className={tab === "settings" ? "tab active" : "tab"} onClick={() => { setTab("settings"); loadSettings(); }}>
          ⚙️
        </button>
      </div>

      {tab === "clips" && (
        <>
          <header>
            <input
              ref={inputRef}
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="搜索… 单击或 Enter 回贴"
              autoFocus
              spellCheck={false}
            />
          </header>

          <div className="toolbar slim">
            <span className="hint">Alt+V 剪贴板 · Alt+T 待办 · 单击回贴 · ↑↓+Enter · 右键 查看/更多 · Esc 隐藏</span>
          </div>

          <div className="list" ref={parentRef}>
            {rows.length === 0 && <div className="empty">暂无记录 — 复制点什么</div>}
            <div style={{ height: total, position: "relative", width: "100%" }}>
              {items.map((vi) => {
                const row = rows[vi.index];
                const sel = row.id === selId;
                return (
                  <div
                    key={row.id}
                    data-index={vi.index}
                    ref={virtualizer.measureElement}
                    className={`row${sel ? " sel" : ""}${expanded === row.id ? " expanded" : ""}`}
                    style={{ position: "absolute", top: 0, left: 0, width: "100%", transform: `translateY(${vi.start}px)` }}
                    onMouseEnter={() => setSelId(row.id)}
                    onClick={() => pasteBack(row.id)} // 单击=回贴并隐藏窗口
                    onContextMenu={(e) => {
                      e.preventDefault();
                      setCtxMenu({ x: e.clientX, y: e.clientY, clipId: row.id });
                    }}
                    title={row.text ?? row.kind}
                  >
                    {row.pinned && <span className="star" title="已收藏">⭐</span>}
                    <span className="kicon">{row.html_path ? "✨" : (KIND_ICON[row.kind] ?? "❔")}</span>
                    {row.kind === "image" && row.image_path
                      ? <img className="thumb" src={convertFileSrc(row.image_path.replace(/\.dib$/, ".png"))} alt="" loading="lazy" />
                      : <span className="rtext">{expanded === row.id || (row.text ?? "").length <= TXT_LEN
                          ? row.text ?? (row.kind === "files" ? row.files_json : "")
                          : (row.text ?? "").slice(0, TXT_LEN) + "…"}</span>}
                    <span className="rmeta">{row.html_path ? "富文本 · " : ""}{relTime(row.ts)}</span>
                    {(row.text ?? "").length > TXT_LEN &&
                      <button className="mini expand" onClick={(e) => { e.stopPropagation(); setExpanded(expanded === row.id ? null : row.id); }}>{expanded === row.id ? "收起" : "展开"}</button>}
                  </div>
                );
              })}
            </div>
          </div>
          {selIdx >= 0 && (
            <footer className="preview">
              <span className="ph">#{rows[selIdx].id} · {rows[selIdx].kind} · {new Date(rows[selIdx].ts).toLocaleString()}</span>
              <pre className="pbody">{rows[selIdx].text ?? rows[selIdx].files_json ?? "(图片)"}</pre>
            </footer>
          )}
        </>
      )}

      {tab === "todos" && (
        <div className="todo-wrap">
          <div className="todo-add">
            <input
              ref={todoInputRef}
              value={todoInput}
              onChange={(e) => setTodoInput(e.target.value)}
              onKeyDown={onTodoKeyDown}
              placeholder="输入待办标题，回车创建"
              spellCheck={false}
              autoFocus
            />
            <button onClick={addTodo}>添加</button>
          </div>

          {/* 筛选 chips */}
          <div className="chips">
            {chips.map((c) => (
              <button
                key={c.key}
                className={filter === c.key && !tagFilter ? "chip active" : "chip"}
                onClick={() => { setFilter(c.key); setTagFilter(""); refreshTodos(); }}
              >
                {c.label} <span className="cnt">{c.count}</span>
              </button>
            ))}
            {Object.entries(todoStats?.tags ?? {}).map(([t, n]) => (
              <button
                key={t}
                className={tagFilter === t ? "chip tag active" : "chip tag"}
                onClick={() => setTagFilter(tagFilter === t ? "" : t)}
              >
                #{t} <span className="cnt">{n}</span>
              </button>
            ))}
          </div>

          <div className="todo-list">
            {todos.length === 0 && <div className="empty">暂无待办 — 添加或右键剪贴板条目「存为待办」</div>}
            {todos.map((t) => {
              const tags: string[] = safeJson(t.tags);
              const hasContent = !!t.content && htmlToText(t.content).length > 0;
              return (
                <div
                  key={t.id}
                  className={t.done ? "trow done" : "trow"}
                  data-calopen={dueFor === t.id ? "1" : undefined}
                  onDoubleClick={() => openEditor(t)}
                >
                  <input type="checkbox" className="cb" checked={t.done} onChange={() => toggleTodo(t.id)} />
                  <div className="tbody">
                    <div className="trow-main">
                      <span
                        className={t.pinned ? "pin on" : "pin"}
                        onClick={() => togglePin(t.id)}
                        title={t.pinned ? "取消置顶" : "置顶"}
                      >📌</span>
                      <span
                        className={"prio p" + t.priority}
                        onClick={() => cyclePriority(t.id, t.priority)}
                        title={`优先级：${PRIO_NAME[t.priority]}（点击切换）`}
                      >{PRIO_ICON[t.priority]}</span>
                      {t.due_at != null && !t.done && (() => {
                        const { label, cls } = dueLabel(t.due_at);
                        return <span className={cls}>{label}</span>;
                      })()}
                      <span className="ttitle" title="双击编辑详情">
                        {t.kind === "image" && t.image_path
                          ? <img className="timg" src={convertFileSrc(t.image_path.replace(/\.dib$/, ".png"))} alt="待办图片" onClick={(e) => { e.stopPropagation(); setViewerImg((e.target as HTMLImageElement).src); }} />
                          : null}
                        <span className="ttxt">{t.title || t.text || ""}</span>
                        {hasContent && <span className="tcontent-dot" title="有详细内容">☰</span>}
                      </span>
                    </div>
                    <div className="tmeta">
                      {tags.length > 0 && (
                        <span className="tagrow">
                          {tags.map((g) => (
                            <span key={g} className="tag" onClick={() => setTagFilter(g)}>#{g}</span>
                          ))}
                        </span>
                      )}
                      {KIND_ICON[t.kind] ?? "❔"} {t.kind !== "text" && `${t.kind} · `}{relTime(t.ts)}
                      {t.done && t.done_at && ` · 完成 ${new Date(t.done_at).toLocaleTimeString()}`}
                    </div>
                  </div>
                  <div className="tacts">
                    <button
                      onClick={(e) => {
                        if (dueFor === t.id) setDueFor(null);
                        else { setDueAnchor(e.currentTarget.getBoundingClientRect()); setDueFor(t.id); }
                      }}
                      title="设置截止日">📅</button>
                    <button onClick={() => openEditor(t)} title="编辑详情（双击行）">编辑</button>
                        <button onClick={() => deleteTodo(t.id)}>删除</button>
                  </div>
                  {dueFor === t.id && dueAnchor && (
                    <CalendarPopover
                      value={t.due_at != null ? new Date(t.due_at).toISOString().slice(0, 10) : null}
                      anchorRect={dueAnchor}
                      onPick={(ymd) => setDue(t.id, ymd)}
                      onClose={() => setDueFor(null)}
                    />
                  )}
                </div>
              );
            })}
          </div>
          <div className="todo-foot">
            <span>双击编辑详情 · Ctrl+V 收任意格式 · Alt+T 直接唤出待办</span>
          </div>
        </div>
      )}

      {tab === "settings" && settings && (
        <div className="settings">
          <div className="set-row">
            <div className="set-label">剪贴板热键</div>
            <input
              className="hk-input"
              readOnly
              value={hkRecording ? "按下组合键…" : (hkInput || "alt+v")}
              onKeyDown={async (e) => {
                if (!hkRecording) return;
                e.preventDefault();
                if (e.key === "Escape") { setHkRecording(false); return; }
                const mods: string[] = [];
                if (e.ctrlKey) mods.push("ctrl");
                if (e.altKey) mods.push("alt");
                if (e.shiftKey) mods.push("shift");
                if (e.metaKey) mods.push("super");
                const key = e.key.toLowerCase();
                if (["control", "alt", "shift", "meta"].includes(key)) return;
                if (mods.length === 0) return;
                const acc = [...mods, key].join("+");
                setHkInput(acc);
                setHkRecording(false);
                try {
                  await invoke("set_hotkey", { accelerator: acc });
                  setBench(`热键已改为 ${acc} ✓`);
                } catch (err) {
                  setBench(`热键设置失败：${err}`);
                  setHkInput(settings.hotkey);
                }
              }}
              onFocus={() => setHkRecording(true)}
              placeholder="点击后按组合键"
            />
            <span className="set-hint">点击输入框后按下新组合键（至少一个修饰键 + 字母/数字）</span>
          </div>

          <div className="set-row">
            <div className="set-label">待办热键</div>
            <input
              className="hk-input"
              readOnly
              value={hkTRecording ? "按下组合键…" : (hkTInput || "alt+t")}
              onKeyDown={async (e) => {
                if (!hkTRecording) return;
                e.preventDefault();
                if (e.key === "Escape") { setHkTRecording(false); return; }
                const mods: string[] = [];
                if (e.ctrlKey) mods.push("ctrl");
                if (e.altKey) mods.push("alt");
                if (e.shiftKey) mods.push("shift");
                if (e.metaKey) mods.push("super");
                const key = e.key.toLowerCase();
                if (["control", "alt", "shift", "meta"].includes(key)) return;
                if (mods.length === 0) return;
                const acc = [...mods, key].join("+");
                setHkTInput(acc);
                setHkTRecording(false);
                try {
                  await invoke("set_hotkey_todos", { accelerator: acc });
                  setBench(`待办热键已改为 ${acc} ✓`);
                } catch (err) {
                  setBench(`待办热键设置失败：${err}`);
                  setHkTInput(settings.hotkey_todos);
                }
              }}
              onFocus={() => setHkTRecording(true)}
              placeholder="点击后按组合键"
            />
            <span className="set-hint">唤出窗口并直接落到待办 Tab</span>
          </div>

          <div className="set-row">
            <div className="set-label">历史容量上限</div>
            <div className="seg">
              {[["2千", 2000], ["1万", 10000], ["10万", 100000]].map(([label, v]) => (
                <button
                  key={String(v)}
                  className={settings.history_limit === v ? "seg-btn active" : "seg-btn"}
                  onClick={async () => {
                    await invoke("set_history_limit", { limit: v });
                    setSettings({ ...settings, history_limit: v as number });
                    refresh(queryRef.current);
                  }}
                >{label as string}</button>
              ))}
            </div>
            <span className="set-hint">超出自动清理最旧的（收藏条目永久保留）</span>
          </div>

          <div className="set-row">
            <div className="set-label">剪贴板监听</div>
            <button
              className={settings.paused ? "seg-btn" : "seg-btn active"}
              onClick={async () => {
                await invoke("set_paused", { paused: !settings.paused });
                setSettings({ ...settings, paused: !settings.paused });
              }}
            >{settings.paused ? "已暂停（点击恢复）" : "监听中（点击暂停）"}</button>
            <span className="set-hint">隐私模式：暂停期间复制的内容不会被记录</span>
          </div>

          <div className="set-row">
            <div className="set-label">开机自启</div>
            <AutoStartToggle />
            <span className="set-hint">随 Windows 启动，静默驻留托盘</span>
          </div>

          <div className="set-row">
            <div className="set-label">数据</div>
            <span className="set-hint">{stats.count} 条历史 · 本地存储，不上传任何数据</span>
          </div>
          {bench && <div className="set-bench">{bench}</div>}
        </div>
      )}
      {/* 待办编辑弹窗 */}
      {editor && (
        <TodoEditor
          draft={editor}
          isNew={!!(editor as TodoEditDraft & { isNew?: boolean }).isNew}
          onField={(patch) => setEditor({ ...editor, ...patch })}
          onSave={saveEditor}
          onClose={() => { setEditor(null); refreshTodos(); }}
        />
      )}

      {/* 图片放大查看器 */}
      {viewerImg && (
        <div className="viewer" onClick={() => setViewerImg(null)}>
          <img src={viewerImg} alt="预览" onClick={(e) => e.stopPropagation()} />
          <div className="viewer-tip">点击空白处关闭</div>
        </div>
      )}

      {/* 右键菜单 */}
      {ctxMenu && (
        <>
          <div style={{ position: "fixed", inset: 0, zIndex: 998 }} onClick={closeCtxMenu} onContextMenu={(e) => { e.preventDefault(); closeCtxMenu(); }} />
          <div className="ctxmenu" style={{ left: Math.min(ctxMenu.x, window.innerWidth - 180), top: Math.min(ctxMenu.y, window.innerHeight - 100) }}>
                        <div className="item" onClick={() => { pasteBack(ctxMenu.clipId); closeCtxMenu(); }}>📋 粘贴此项</div>
            <div className="item" onClick={() => { ctxView(ctxMenu.clipId); closeCtxMenu(); }}>👁 查看 / 展开</div>
            <div className="item" onClick={() => clipTogglePin(ctxMenu.clipId)}>⭐ 收藏 / 取消收藏</div>
            <div className="item" onClick={() => saveClipAsTodoAndEdit(ctxMenu.clipId)}>✅ 存为待办</div>
            <div className="item" onClick={() => clipDelete(ctxMenu.clipId)}>🗑️ 删除此项</div>
            <div className="item danger" onClick={() => clipsClear()}>🧹 清空历史（保留收藏）</div>
          </div>
        </>
      )}
    </div>
  );
}
