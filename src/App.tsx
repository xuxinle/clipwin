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
  done_today: number;
  tags: Record<string, number>;
}

const KIND_ICON: Record<string, string> = { text: "📝", image: "🖼️", files: "📁" };

/** 列表分组：key 稳定（用于折叠状态记忆），顺序即展示顺序 */
const GROUP_ORDER = ["pin", "overdue", "today", "tomorrow", "later", "nodate", "done"] as const;
type GroupKey = (typeof GROUP_ORDER)[number];
const GROUP_LABEL: Record<GroupKey, string> = {
  pin: "📌 置顶", overdue: "⚠️ 逾期", today: "🔥 今天",
  tomorrow: "🌤 明天", later: "📅 后续", nodate: "⏳ 无日期", done: "✅ 已完成",
};

/** 按状态把（后端已排序的）待办切成分组，组内保持后端顺序 */
function groupTodos(rows: TodoRow[]): { key: GroupKey; label: string; items: TodoRow[] }[] {
  const now = new Date();
  const today0 = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
  const b: Record<GroupKey, TodoRow[]> = { pin: [], overdue: [], today: [], tomorrow: [], later: [], nodate: [], done: [] };
  for (const t of rows) {
    if (t.done) { b.done.push(t); continue; }
    if (t.pinned) { b.pin.push(t); continue; }
    if (t.due_at == null) { b.nodate.push(t); continue; }
    const d = new Date(t.due_at);
    const diff = Math.round((new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime() - today0) / 86_400_000);
    if (diff < 0) b.overdue.push(t);
    else if (diff === 0) b.today.push(t);
    else if (diff === 1) b.tomorrow.push(t);
    else b.later.push(t);
  }
  return GROUP_ORDER.filter((k) => b[k].length > 0).map((k) => ({ key: k, label: GROUP_LABEL[k], items: b[k] }));
}

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

/** 热键唤出时的默认预选行下标：1 = 列表第二项。
 *
 * 为什么不是首项（下标 0）：回贴会把用过的条目置顶，所以下次唤出时
 * 下标 0 往往正是上一次刚回贴过的内容；第二项才是高频复用项。
 * 预选它可省掉一次 ↓ —— 唤出后直接 Enter 即回贴，减少一次按键。
 * 列表不足 2 项时自动回退到首项（不会越界）。 */
const POPUP_SELECT_INDEX = 1;

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
  const [todoQuery, setTodoQuery] = useState(""); // 待办搜索（标题/内容/标签）
  const [todoSort, setTodoSort] = useState<"smart" | "due" | "prio" | "created">("smart");
  const [todoSelId, setTodoSelId] = useState<number | null>(null); // 键盘选中行
  const [collapsed, setCollapsed] = useState<Record<string, boolean>>({}); // 分组折叠状态
  // 延迟删除：先在前端隐藏并进入待提交队列，5 秒内可撤销；超时/隐藏窗口才真正落库
  const [pendingDel, setPendingDel] = useState<TodoRow[]>([]);
  const [undoBar, setUndoBar] = useState(0); // >0 时展示「已删除 N 项 · 撤销」
  const delTimer = useRef<number | undefined>(undefined);
  const pendingDelRef = useRef<TodoRow[]>([]);
  const todoQueryTimer = useRef<number | undefined>(undefined);
  const refreshTodosRef = useRef<() => void>(() => {});
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

  /** preferIdx 非空时强制预选该下标（热键唤出用），否则沿用旧选中/回退首项 */
  async function refresh(q: string, preferIdx?: number) {
    try {
      const r = await invoke<ClipRow[]>("search", { query: q, limit: 10000 });
      setRows(r);
      if (preferIdx != null && r.length > 0) {
        const i = Math.min(preferIdx, r.length - 1);
        setSelId(r[i].id);
        virtualizer.scrollToIndex(i, { align: "auto" });
      } else {
        setSelId((prev) => (prev != null && r.some((x) => x.id === prev) ? prev : r[0]?.id ?? null));
      }
        const s = await invoke<{ count: number }>("stats");
      setStats(s);
    } catch (e) {
      setBench(`错误：${e}`);
    }
  }

  async function refreshTodos() {
    try {
      const r = await invoke<TodoRow[]>("todos_list", { filter, tag: tagFilter, includeDone: false, query: todoQuery, sort: todoSort });
      setTodos(r);
      // 键盘选中行：优先保留，其次落到首行（无选中且列表非空时）
      setTodoSelId((prev) => (prev != null && r.some((x) => x.id === prev) ? prev : r[0]?.id ?? null));
      const s = await invoke<TodoStats>("todos_stats");
      setTodoStats(s);
    } catch (e) {
      setBench(`待办读取失败：${e}`);
    }
  }
  refreshTodosRef.current = refreshTodos;

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
  // 屏蔽 WebView 浏览器默认右键菜单（全应用，任何位置）
  useEffect(() => {
    const noCtx = (e: MouseEvent) => e.preventDefault();
    document.addEventListener("contextmenu", noCtx);
    return () => document.removeEventListener("contextmenu", noCtx);
  }, []);

  useEffect(() => {
    refresh("");
    const un1 = listen("clip:new", () => refresh(queryRef.current));
    // 唤出事件：payload.tab 指定落在哪个 Tab（Ctrl+Alt+Q=clips / Ctrl+Alt+W=todos）
    const un2 = listen<{ tab: string }>("clipwin:popup", (ev) => {
      const t = ev.payload?.tab === "todos" ? "todos" : "clips";
      if (t !== tabRef.current) setTab(t);
      // 热键唤出必刷新：回贴置顶/新增立即反映（visibilitychange 在 WebView2 不可靠）
      refresh(t === "clips" ? queryRef.current : "", t === "clips" ? POPUP_SELECT_INDEX : undefined);
      if (t === "todos") refreshTodos();
      if (t === "clips") focusInput();
      else todoInputRef.current?.focus();
    });
    // 窗口从隐藏到可见（托盘/热键唤出）：重新聚焦
    const onVis = () => {
      if (document.visibilityState === "visible") {
        // 每次唤出重拉列表（回贴置顶/新增条目立即反映）；剪贴板页预选第二项
        refresh(queryRef.current, tabRef.current === "clips" ? POPUP_SELECT_INDEX : undefined);
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

  useEffect(() => { refreshTodos(); }, [filter, tagFilter, todoSort]);
  // 搜索词防抖 150ms（与剪贴板搜索同一手感）
  useEffect(() => {
    window.clearTimeout(todoQueryTimer.current);
    todoQueryTimer.current = window.setTimeout(() => refreshTodos(), 150);
    return () => window.clearTimeout(todoQueryTimer.current);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [todoQuery]);

  // 窗口隐藏 / 组件卸载时立即把「延迟删除」落库，避免撤销窗口内关窗导致删除丢失
  useEffect(() => {
    const onHide = () => { if (document.visibilityState === "hidden") void flushDeletes(); };
    document.addEventListener("visibilitychange", onHide);
    return () => {
      document.removeEventListener("visibilitychange", onHide);
      void flushDeletes();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

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
  /** 新建待办（标题模式）：输入框内容，支持 #标签 !1!2!3 优先级快捷语法。
   *  editAfter=false（默认，回车）→ 只创建，不打断录入手感；
   *  editAfter=true（Ctrl+Enter）→ 创建后打开编辑器补详细内容。 */
  async function addTodo(editAfter = false) {
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
      refreshTodos();
      if (editAfter) openEditor(row, true); // Ctrl+Enter：直接进详情编辑
      else {
        setTodoSelId(row.id); // 新条目成为选中行，可继续 ↓ 或回车编辑
        todoInputRef.current?.focus();
      }
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
    const row = todos.find((t) => t.id === id);
    if (row) stageDelete([row]);
  }

  // ---------- 延迟删除 + 撤销 ----------
  // 删除不立即落库：先把行从列表中隐藏并进入待提交队列，5 秒内可点「撤销」恢复；
  // 超时、切换筛选/Tab、窗口隐藏时才真正调 todos_delete_many。
  // 好处：误删可救；附件不会被提前按引用计数清理。

  /** 把待提交队列真正落库（幂等：队列空时什么都不做） */
  async function flushDeletes() {
    window.clearTimeout(delTimer.current);
    const rows = pendingDelRef.current;
    if (rows.length === 0) return;
    pendingDelRef.current = [];
    setPendingDel([]);
    setUndoBar(0);
    try {
      await invoke("todos_delete_many", { ids: rows.map((r) => r.id) });
    } catch (e) {
      setBench(`删除失败：${e}`);
    }
    refreshTodosRef.current();
  }

  function stageDelete(rows: TodoRow[]) {
    if (rows.length === 0) return;
    const merged = [...pendingDelRef.current, ...rows];
    pendingDelRef.current = merged;
    setPendingDel(merged);
    setUndoBar(merged.length);
    window.clearTimeout(delTimer.current);
    delTimer.current = window.setTimeout(() => { void flushDeletes(); }, 5000);
  }

  /** 撤销：清空待提交队列，条目从未离开数据库，直接恢复显示即可 */
  function undoDelete() {
    window.clearTimeout(delTimer.current);
    pendingDelRef.current = [];
    setPendingDel([]);
    setUndoBar(0);
  }

  /** 一键清除已完成：同样走延迟删除（先取全部已完成行，不局限当前筛选） */
  async function clearDone() {
    try {
      const all = await invoke<TodoRow[]>("todos_list", { filter: "done", tag: "", includeDone: false, query: "", sort: "smart" });
      if (all.length === 0) return;
      stageDelete(all);
    } catch (e) {
      setBench(`清除失败：${e}`);
    }
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

  /** 待办视图键盘（文档级监听）：输入框内 Enter 添加；其余支持列表导航与快捷操作。
   *
   * 为何用文档级而非容器 onKeyDown：行被删除/完成后会从 DOM 移除，
   * 焦点回落 body，容器上的处理器便再也收不到事件（键盘流中断）。 */
  function onTodoKeyDown(e: KeyboardEvent) {
    if (e.isComposing || e.defaultPrevented) return;
    if (editor) return; // 编辑弹窗打开时交给弹窗自己处理
    const el = e.target as HTMLElement | null;
    const tag = el?.tagName;
    const inField = !!el && (tag === "INPUT" || tag === "TEXTAREA" || el.isContentEditable);

    // ① 添加框内 Enter：直接创建（不打断），Ctrl+Enter 创建并进详情
    if (e.key === "Enter" && el === todoInputRef.current) {
      e.preventDefault();
      void addTodo(e.ctrlKey || e.metaKey);
      return;
    }
    // ② Esc：优先撤销删除提示，否则隐藏窗口
    if (e.key === "Escape") {
      e.preventDefault();
      if (undoBar > 0) undoDelete();
      else if (dueFor != null) setDueFor(null);
      else void invoke("hide_window");
      return;
    }
    if (tag === "SELECT") return; // 排序下拉：不拦截
    // ③ ↑↓：单行输入框/列表都用不到上下键，一律让给列表导航
    if (e.key === "ArrowDown") { e.preventDefault(); moveTodoSel(1); return; }
    if (e.key === "ArrowUp") { e.preventDefault(); moveTodoSel(-1); return; }
    if (inField) return; // 输入框内的空格/退格交给原生编辑

    // ④ 列表快捷键
    if (e.key === " " && todoSelId != null) {
      e.preventDefault();
      const idx = visibleTodos.findIndex((t) => t.id === todoSelId);
      void toggleTodo(todoSelId);
      // 完成项在「进行中」筛选下会移出视图，选中顺位下移，保持连续勾选手感
      const next = visibleTodos[idx + 1] ?? visibleTodos[idx - 1];
      if (next) setTodoSelId(next.id);
    } else if ((e.key === "Delete" || e.key === "Backspace") && todoSelId != null) {
      e.preventDefault();
      deleteTodo(todoSelId);
      moveTodoSel(1);
    } else if (e.key === "Enter" && todoSelId != null) {
      e.preventDefault();
      const t = todos.find((x) => x.id === todoSelId);
      if (t) openEditor(t);
    }
  }

  // 待办 Tab 下挂文档级键盘监听；handler 经 ref 转发，避免每次渲染重绑
  const todoKeyRef = useRef<(e: KeyboardEvent) => void>(() => {});
  todoKeyRef.current = onTodoKeyDown;
  useEffect(() => {
    if (tab !== "todos") return;
    const h = (e: KeyboardEvent) => todoKeyRef.current(e);
    document.addEventListener("keydown", h, true);
    return () => document.removeEventListener("keydown", h, true);
  }, [tab]);

  /** 键盘选中行上下移动（按当前可见分组展平后的顺序，跳过待删行） */
  function moveTodoSel(delta: number) {
    const flat = visibleTodos;
    if (flat.length === 0) return;
    const cur = flat.findIndex((t) => t.id === todoSelId);
    const next = cur === -1
      ? (delta > 0 ? 0 : flat.length - 1)
      : Math.min(flat.length - 1, Math.max(0, cur + delta));
    setTodoSelId(flat[next].id);
    document.querySelector(`[data-tid="${flat[next].id}"]`)?.scrollIntoView({ block: "nearest" });
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

  // ---- 待办派生数据 ----
  // 待删行立即从界面消失（但仍在 DB，可撤销）；分组基于可见行计算
  const visibleTodos = todos.filter((t) => !pendingDel.some((p) => p.id === t.id));
  const todoGroups = groupTodos(visibleTodos);
  const searching = todoQuery.trim().length > 0;
  // 智能排序 = 分组视图（按紧迫度分组）；选了其他排序则改平铺，否则排序会被分组淹没
  const useGroups = todoSort === "smart";
  // 今日完成进度：已完成(今日) / (已完成(今日) + 未完成)
  const doneToday = todoStats?.done_today ?? 0;
  const openCnt = todoStats?.open ?? 0;
  const progress = doneToday + openCnt > 0 ? Math.round((doneToday / (doneToday + openCnt)) * 100) : 0;

  /** 待办单行渲染（分组列表共用）；选中行走高亮，鼠标悬停也同步选中便于键盘接力 */
  function todoRow(t: TodoRow) {
    const tags: string[] = safeJson(t.tags);
    const hasContent = !!t.content && htmlToText(t.content).length > 0;
    const sel = todoSelId === t.id;
    return (
      <div
        key={t.id}
        data-tid={t.id}
        className={`trow${t.done ? " done" : ""}${sel ? " sel" : ""}`}
        data-calopen={dueFor === t.id ? "1" : undefined}
        tabIndex={-1}
        onMouseEnter={() => setTodoSelId(t.id)}
        onClick={(e) => {
          // 点击行就地把焦点移到行上：否则焦点留在搜索/添加框，Del、空格会被当成文本编辑吞掉
          setTodoSelId(t.id);
          if (e.detail === 1) (e.currentTarget as HTMLElement).focus();
        }}
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
          <button onClick={() => deleteTodo(t.id)} title="删除（5 秒内可撤销）">删除</button>
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
  }

  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => parentRef.current,
    estimateSize: () => 64,
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
          {/* 快速添加：回车直接创建（不弹窗打断），Ctrl+Enter 创建并进详情 */}
          <div className="todo-add">
            <input
              ref={todoInputRef}
              value={todoInput}
              onChange={(e) => setTodoInput(e.target.value)}
              placeholder="输入待办标题，回车创建（支持 #标签 !1 优先级）"
              spellCheck={false}
              autoFocus
            />
            <button onClick={() => addTodo()} title="回车同效">添加</button>
          </div>

          {/* 统计条：未完成数 / 逾期·今日提醒 / 今日完成进度条 */}
          <div className="todo-stat">
            <span className="ts-num">{openCnt}</span>
            <span className="ts-label">项未完成</span>
            {(todoStats?.overdue ?? 0) > 0 && <span className="ts-badge od">⚠️ 逾期 {todoStats!.overdue}</span>}
            {(todoStats?.today ?? 0) > 0 && <span className="ts-badge td">🔥 今天 {todoStats!.today}</span>}
            <span className="ts-spacer" />
            <span className="ts-pct" title={`今日完成 ${doneToday} 项`}>今日完成 {doneToday}</span>
            <span className="ts-bar" title={`完成度 ${progress}%`}><i style={{ width: `${progress}%` }} /></span>
          </div>

          {/* 搜索 + 排序 */}
          <div className="todo-tools">
            <input
              className="todo-search"
              value={todoQuery}
              onChange={(e) => setTodoQuery(e.target.value)}
              placeholder="🔍 搜索标题 / 内容 / 标签…"
              spellCheck={false}
            />
            <select
              className="todo-sort"
              value={todoSort}
              onChange={(e) => setTodoSort(e.target.value as "smart" | "due" | "prio" | "created")}
              title="排序方式"
            >
              <option value="smart">智能排序</option>
              <option value="due">按截止日</option>
              <option value="prio">按优先级</option>
              <option value="created">按创建时间</option>
            </select>
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
            {visibleTodos.length === 0 && (
              <div className="empty todo-empty">
                {searching || tagFilter ? (
                  <>
                    <div className="ee-t">没有匹配的待办</div>
                    <div className="ee-s">试试别的关键词，或清空搜索 / 标签筛选</div>
                  </>
                ) : (
                  <>
                    <div className="ee-t">还没有待办</div>
                    <div className="ee-s">在上方输入框写一条，回车即可创建</div>
                    <div className="ee-tips">
                      <span><code>#工作</code> 加标签</span>
                      <span><code>!1</code> <code>!2</code> <code>!3</code> 设优先级</span>
                      <span><code>Ctrl+V</code> 把剪贴板内容存为待办</span>
                    </div>
                  </>
                )}
              </div>
            )}
            {useGroups && todoGroups.map((g) => {
              const isCollapsed = !!collapsed[g.key];
              return (
                <div className="tgroup" key={g.key}>
                  <div
                    className={isCollapsed ? "tgroup-hd collapsed" : "tgroup-hd"}
                    onClick={() => setCollapsed((c) => ({ ...c, [g.key]: !c[g.key] }))}
                    title={isCollapsed ? "展开分组" : "折叠分组"}
                  >
                    <span className="tg-caret">{isCollapsed ? "▸" : "▾"}</span>
                    <span className="tg-label">{g.label}</span>
                    <span className="tg-count">{g.items.length}</span>
                  </div>
                  {!isCollapsed && g.items.map((t) => todoRow(t))}
                </div>
              );
            })}
            {!useGroups && visibleTodos.map((t) => todoRow(t))}
          </div>
          <div className="todo-foot">
            {undoBar > 0 ? (
              <div className="undo-bar">
                <span className="ub-text">已删除 {undoBar} 项</span>
                <button className="undo-btn" onClick={undoDelete}>撤销</button>
                <span className="ub-hint">（5 秒后自动确认）</span>
              </div>
            ) : (
              <>
                <span className="foot-hint">
                  ↑↓ 选择 · Enter 编辑 · 空格 完成 · Del 删除 · Ctrl+V 存剪贴板
                </span>
                <button
                  className="foot-clear"
                  disabled={(todoStats?.done ?? 0) === 0}
                  onClick={clearDone}
                  title="删除所有已完成待办（5 秒内可撤销）"
                >清除已完成{(todoStats?.done ?? 0) > 0 ? ` (${todoStats!.done})` : ""}</button>
              </>
            )}
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
              value={hkRecording ? "按下组合键…" : (hkInput || "ctrl+alt+q")}
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
              value={hkTRecording ? "按下组合键…" : (hkTInput || "ctrl+alt+w")}
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
