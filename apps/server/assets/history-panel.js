(() => {
  const content = document.querySelector('#content');
  const title = document.querySelector('#title');
  if (!content || !title) return;

  const esc = (value) => String(value ?? '').replace(/[&<>"']/g, (c) => ({
    '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;'
  }[c]));
  const fmt = (value) => new Date(value).toLocaleString('zh-CN', { hour12: false });
  const stateText = (state) => ({
    queued: '等待解析', parsing: '解析中', completed: '已完成',
    failed: '解析失败', timed_out: '解析超时', cancelled: '已取消'
  }[state] || '暂无解析');

  // Settings 页不依赖外部 Markdown CDN；只渲染常见 Markdown，并先转义原文。
  const renderMarkdown = (source) => {
    const lines = String(source || '').replace(/\r\n?/g, '\n').split('\n');
    const inline = (value) => {
      let text = esc(value);
      const code = [];
      text = text.replace(/`([^`\n]+)`/g, (_, value) => {
        code.push(`<code>${value}</code>`);
        return `\u0000${code.length - 1}\u0000`;
      });
      text = text.replace(/\*\*(.+?)\*\*|__(.+?)__/g, (_, a, b) => `<strong>${a || b}</strong>`);
      text = text.replace(/\[([^\]]+)\]\((https?:\/\/[^\s)]+)\)/g, '<a href="$2" target="_blank" rel="noopener noreferrer">$1</a>');
      text = text.replace(/\u0000(\d+)\u0000/g, (_, index) => code[Number(index)]);
      return text;
    };
    const html = [];
    let list = null;
    let code = null;
    const closeList = () => { if (list) { html.push(`</${list}>`); list = null; } };
    const closeCode = () => { if (code) { html.push(`<pre><code>${esc(code.join('\n'))}</code></pre>`); code = null; } };
    for (const raw of lines) {
      const line = raw.trim();
      if (code) { if (line.startsWith('```')) closeCode(); else code.push(raw); continue; }
      if (line.startsWith('```')) { closeList(); code = []; continue; }
      if (!line) { closeList(); continue; }
      const heading = line.match(/^(#{1,6})\s+(.+)$/);
      if (heading) { closeList(); html.push(`<h${heading[1].length}>${inline(heading[2])}</h${heading[1].length}>`); continue; }
      const item = line.match(/^([-*+])\s+(.+)$/);
      if (item) { if (list !== 'ul') { closeList(); html.push('<ul>'); list = 'ul'; } html.push(`<li>${inline(item[2])}</li>`); continue; }
      const ordered = line.match(/^\d+[.)]\s+(.+)$/);
      if (ordered) { if (list !== 'ol') { closeList(); html.push('<ol>'); list = 'ol'; } html.push(`<li>${inline(ordered[1])}</li>`); continue; }
      if (line.startsWith('>')) { closeList(); html.push(`<blockquote>${inline(line.replace(/^>\s?/, ''))}</blockquote>`); continue; }
      closeList(); html.push(`<p>${inline(line)}</p>`);
    }
    closeCode(); closeList(); return html.join('');
  };

  const style = document.createElement('style');
  style.textContent = `
    .history-browser{display:block}
    .history-browser>.card{min-width:0}
    .history-list{max-height:calc(100vh - 230px);overflow:auto}
    .history-list article{align-items:flex-start;min-height:110px;padding:14px 8px;gap:12px}
    .history-list article.selected{background:#edf4ff;box-shadow:inset 3px 0 var(--blue)}
    .history-list article:focus-visible{outline:3px solid rgba(63,124,255,.3);outline-offset:2px}
    .history-summary{display:block;margin-top:7px;color:var(--muted);line-height:1.45;display:-webkit-box;-webkit-line-clamp:3;-webkit-box-orient:vertical;overflow:hidden}
    .history-status{display:inline-block;margin-top:6px;padding:2px 7px;border-radius:999px;background:#f1f5fb;color:var(--muted);font-size:12px}
    .history-modal{position:fixed;inset:0;z-index:1000;display:grid;place-items:center;padding:28px;background:rgba(20,33,61,.46);backdrop-filter:blur(8px);animation:history-modal-in .16s ease-out}
    .history-dialog{position:relative;width:min(1180px,96vw);max-height:92vh;overflow:hidden;border:1px solid rgba(255,255,255,.72);border-radius:22px;background:var(--panel);box-shadow:0 28px 80px rgba(20,33,61,.28)}
    .history-dialog-head{display:flex;align-items:center;justify-content:space-between;gap:16px;padding:18px 22px;border-bottom:1px solid var(--line)}
    .history-dialog-head h3{margin:0;font-size:18px;text-wrap:balance}.history-dialog-head small{color:var(--muted)}
    .history-close{display:grid;place-items:center;width:44px;height:44px;border:1px solid var(--line);border-radius:12px;background:#fff;color:var(--muted);font-size:22px;line-height:1;cursor:pointer;transition:background-color .15s,color .15s,transform .15s}.history-close:hover{background:#edf4ff;color:var(--blue)}.history-close:active{transform:scale(.96)}
    .history-detail-grid{display:grid;grid-template-columns:minmax(280px,43%) minmax(0,57%);gap:22px;padding:22px;max-height:calc(92vh - 84px);overflow:auto;align-items:start}
    .history-detail .shot{min-height:330px;max-height:calc(92vh - 180px)}
    .history-image-trigger{display:block;width:100%;height:100%;padding:0;border:0;background:transparent;cursor:zoom-in}
    .history-image-trigger:focus-visible{outline:3px solid rgba(63,124,255,.45);outline-offset:-3px}
    .history-detail .shot img{max-height:calc(92vh - 220px);pointer-events:none}
    .history-group-controls{display:flex;align-items:center;justify-content:center;gap:10px;margin-top:10px;color:var(--muted);font-size:12px}
    .history-group-controls button{min-width:72px;height:32px;border:1px solid var(--line);border-radius:8px;background:#fff;color:var(--blue);cursor:pointer}
    .history-group-controls button:disabled{opacity:.4;cursor:default}
    .history-group-controls[hidden]{display:none}
    .history-markdown{font-size:15px;line-height:1.65;color:#283957;max-height:calc(92vh - 180px);overflow:auto;padding-right:8px;text-wrap:pretty}
    .history-lightbox{position:fixed;inset:0;z-index:1100;display:flex;flex-direction:column;background:rgba(8,17,35,.92);color:#fff;animation:history-modal-in .16s ease-out}
    .history-lightbox-head{display:flex;align-items:center;justify-content:space-between;gap:14px;padding:16px 22px;background:rgba(8,17,35,.72)}
    .history-lightbox-head strong{font-size:15px}.history-lightbox-head small{color:#b8c6df}
    .history-lightbox-close{display:grid;place-items:center;width:44px;height:44px;border:1px solid rgba(255,255,255,.26);border-radius:12px;background:rgba(255,255,255,.08);color:#fff;font-size:22px;cursor:pointer}
    .history-lightbox-stage{display:grid;place-items:center;flex:1;min-height:0;overflow:hidden;touch-action:none;cursor:grab}
    .history-lightbox-stage:active{cursor:grabbing}.history-lightbox-stage img{max-width:92vw;max-height:calc(100vh - 132px);object-fit:contain;transform-origin:center;user-select:none;will-change:transform}
    .history-lightbox-help{padding:10px 22px 16px;text-align:center;color:#b8c6df;font-size:12px}
    @keyframes history-modal-in{from{opacity:0}to{opacity:1}}
    .history-markdown h1,.history-markdown h2,.history-markdown h3{color:#1d4ed8;margin:10px 0 6px}.history-markdown h1:first-child{margin-top:0}
    .history-markdown p{margin:0 0 8px}.history-markdown ul,.history-markdown ol{margin:4px 0 8px 22px;padding:0}.history-markdown li{margin:2px 0}
    .history-markdown blockquote{margin:6px 0 10px;padding:7px 10px;border-left:3px solid #bcd1ff;background:#f3f7ff;color:#38507b}
    .history-markdown pre{padding:9px 10px;overflow:auto;background:#101a2b;color:#e8eef9;border-radius:8px}.history-markdown code{font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;font-size:12px}.history-markdown a{color:#2563c7}
    @media(max-width:900px){.history-modal{padding:12px}.history-dialog{width:100%;max-height:94vh;border-radius:18px}.history-dialog-head{padding:14px 16px}.history-detail-grid{grid-template-columns:1fr;padding:16px;gap:14px;max-height:calc(94vh - 76px)}.history-list{max-height:none}.history-detail .shot{min-height:220px;max-height:38vh}.history-detail .shot img{max-height:34vh}.history-markdown{max-height:none;padding-right:0}.history-lightbox-head{padding:12px 14px}.history-lightbox-stage img{max-width:96vw;max-height:calc(100vh - 118px)}}
    @media(prefers-reduced-motion:reduce){.history-modal{animation:none}.history-close{transition:none}}
  `;
  document.head.appendChild(style);

  let selected = null;
  let currentItems = [];
  const imageUrl = (item) => `/api/v1/captures/${encodeURIComponent(item.capture_id)}/image?v=${encodeURIComponent(item.received_at)}`;
  const detail = (item, answer, status) => `
    <div class="history-modal" role="presentation" data-history-modal>
      <section class="history-dialog" role="dialog" aria-modal="true" aria-labelledby="history-dialog-title">
        <header class="history-dialog-head"><div><h3 id="history-dialog-title">截图与完整解析</h3><small>${esc(item.device_id)} · ${fmt(item.received_at)}</small></div><button class="history-close" type="button" aria-label="关闭详情" data-history-close>×</button></header>
      <div class="history-detail-grid">
        <div><div class="shot"><button class="history-image-trigger" type="button" data-history-zoom aria-label="放大查看 ${esc(item.device_id)} 截图"><img data-history-detail-image src="${imageUrl(item)}" alt="${esc(item.device_id)} 截图"></button></div><div class="history-group-controls" data-history-group-controls ${item.group_page_count > 1 ? '' : 'hidden'}><button type="button" data-history-group-prev>上一张</button><span data-history-group-label>第 1 / ${Math.max(1, item.group_page_count)} 张</span><button type="button" data-history-group-next>下一张</button></div><div class="meta" data-history-detail-meta><span>${esc(item.device_id)}</span><span>${fmt(item.received_at)}</span></div></div>
        <div><div class="history-status" data-history-status>${esc(stateText(status))}</div><article class="history-markdown" data-history-answer>${answer ? renderMarkdown(answer) : '<p>正在加载完整解析…</p>'}</article></div>
      </div>
      </section>
    </div>`;

  const closeLightbox = () => document.querySelector('[data-history-lightbox]')?.remove();
  const closeModal = () => { closeLightbox(); document.querySelector('[data-history-modal]')?.remove(); };

  const openLightbox = (item) => {
    closeLightbox();
    document.body.insertAdjacentHTML('beforeend', `<div class="history-lightbox" role="dialog" aria-modal="true" aria-label="放大查看截图" data-history-lightbox><header class="history-lightbox-head"><div><strong>${esc(item.device_id)} 截图</strong><br><small>滚轮缩放 · 双击切换 · 拖拽查看细节</small></div><button type="button" class="history-lightbox-close" aria-label="关闭图片预览" data-history-lightbox-close>×</button></header><div class="history-lightbox-stage" data-history-lightbox-stage><img src="${imageUrl(item)}" alt="${esc(item.device_id)} 放大截图" draggable="false" data-history-lightbox-image></div><div class="history-lightbox-help">按 Esc 关闭预览</div></div>`);
    const lightbox = document.querySelector('[data-history-lightbox]');
    const close = lightbox.querySelector('[data-history-lightbox-close]');
    const stage = lightbox.querySelector('[data-history-lightbox-stage]');
    const image = lightbox.querySelector('[data-history-lightbox-image]');
    let scale = 1;
    let offsetX = 0;
    let offsetY = 0;
    let pointer = null;
    const update = () => { image.style.transform = `translate(${offsetX}px,${offsetY}px) scale(${scale})`; };
    const reset = () => { scale = 1; offsetX = 0; offsetY = 0; update(); };
    close.addEventListener('click', closeLightbox);
    lightbox.addEventListener('click', (event) => { if (event.target === lightbox) closeLightbox(); });
    stage.addEventListener('wheel', (event) => { event.preventDefault(); scale = Math.min(4, Math.max(1, scale * (event.deltaY < 0 ? 1.15 : 0.87))); if (scale === 1) { offsetX = 0; offsetY = 0; } update(); }, { passive: false });
    stage.addEventListener('dblclick', () => { if (scale === 1) scale = 2.5; else reset(); update(); });
    stage.addEventListener('pointerdown', (event) => { if (scale <= 1) return; pointer = {id:event.pointerId, x:event.clientX, y:event.clientY}; stage.setPointerCapture(event.pointerId); });
    stage.addEventListener('pointermove', (event) => { if (!pointer || pointer.id !== event.pointerId) return; offsetX += event.clientX - pointer.x; offsetY += event.clientY - pointer.y; pointer.x = event.clientX; pointer.y = event.clientY; update(); });
    const release = (event) => { if (pointer?.id === event.pointerId) pointer = null; };
    stage.addEventListener('pointerup', release); stage.addEventListener('pointercancel', release);
    close.focus();
  };

  const selectItem = async (item, node) => {
    selected = item.capture_id;
    document.querySelectorAll('.history-list article').forEach((entry) => entry.classList.toggle('selected', entry === node));
    closeModal();
    document.body.insertAdjacentHTML('beforeend', detail(item, '', item.answer_status));
    const modal = document.querySelector('[data-history-modal]');
    const close = modal.querySelector('[data-history-close]');
    close.addEventListener('click', closeModal);
    modal.addEventListener('click', (event) => { if (event.target === modal) closeModal(); });
    const zoom = modal.querySelector('[data-history-zoom]');
    zoom.addEventListener('click', () => openLightbox(item));
    zoom.addEventListener('keydown', (event) => { if (event.key === 'Enter' || event.key === ' ') { event.preventDefault(); openLightbox(item); } });
    close.focus();
    try {
      const response = await fetch(`/api/v1/captures/${encodeURIComponent(item.capture_id)}/analysis`, { cache: 'no-store' });
      if (!response.ok) throw new Error('analysis unavailable');
      const snapshot = await response.json();
      const target = document.querySelector('[data-history-answer]');
      const state = document.querySelector('[data-history-status]');
      if (target && selected === item.capture_id) {
        updateGroupDetail(item, snapshot.group);
        target.innerHTML = snapshot.task?.answer ? renderMarkdown(snapshot.task.answer) : '<p>暂无完整解析结果。</p>';
        if (state) state.textContent = stateText(snapshot.task?.status || item.answer_status);
      }
    } catch (_) {
      const target = document.querySelector('[data-history-answer]');
      const state = document.querySelector('[data-history-status]');
      if (target && selected === item.capture_id) target.innerHTML = '<p>完整解析读取失败，请关闭后重试。</p>';
      if (state) state.textContent = '读取失败';
    }
  };

  const updateGroupDetail = (item, group) => {
    const modal = document.querySelector('[data-history-modal]');
    if (!modal || !group?.captures?.length) return;
    let index = Math.max(0, group.captures.findIndex((capture) => capture.id === item.capture_id));
    if (index < 0) index = 0;
    const image = modal.querySelector('[data-history-detail-image]');
    const meta = modal.querySelector('[data-history-detail-meta]');
    const controls = modal.querySelector('[data-history-group-controls]');
    const label = modal.querySelector('[data-history-group-label]');
    const previous = modal.querySelector('[data-history-group-prev]');
    const next = modal.querySelector('[data-history-group-next]');
    if (!image || !meta || !controls || !label || !previous || !next) return;
    controls.hidden = group.captures.length <= 1;
    const show = () => {
      const capture = group.captures[index];
      const current = { ...item, capture_id: capture.id, received_at: capture.received_at, device_id: capture.device_id };
      image.src = imageUrl(current);
      image.alt = `${current.device_id} 截图`;
      meta.innerHTML = `<span>${esc(current.device_id)}</span><span>${fmt(current.received_at)}</span>`;
      label.textContent = `第 ${index + 1} / ${group.captures.length} 张`;
      previous.disabled = index === 0;
      next.disabled = index === group.captures.length - 1;
      const zoom = modal.querySelector('[data-history-zoom]');
      zoom.onclick = () => openLightbox(current);
    };
    previous.onclick = () => { if (index > 0) { index -= 1; show(); } };
    next.onclick = () => { if (index < group.captures.length - 1) { index += 1; show(); } };
    show();
  };

  const render = (page, pageSize, total) => {
    const rows = currentItems.map((item) => `
      <article role="button" tabindex="0" data-history-id="${esc(item.capture_id)}" class="${selected === item.capture_id ? 'selected' : ''}">
        <img class="thumb" src="${imageUrl(item)}" loading="lazy" alt="${esc(item.device_id)} 截图">
        <span><b>${esc(item.device_id)}</b><br><small>${fmt(item.received_at)} · ${item.task_count} 个任务${item.group_page_count > 0 ? ` · 题目组 ${item.group_page_count} 页` : ''}</small><span class="history-status">${item.group_page_count > 0 ? '题目组' : '单题'} · ${esc(stateText(item.answer_status))}</span><span class="history-summary">${esc(item.answer_preview || '暂无解析摘要')}</span></span>
      </article>`).join('');
    content.innerHTML = `<div class="history-browser"><section class="card list history-list"><h3>图片历史</h3>${rows || '<p class="meta">暂无历史记录</p>'}<div class="pager"><button data-history-prev ${page <= 1 ? 'disabled' : ''}>上一页</button><span>第 ${page} 页 / ${Math.max(1, Math.ceil(total / pageSize))}</span><button data-history-next ${page >= Math.max(1, Math.ceil(total / pageSize)) ? 'disabled' : ''}>下一页</button></div></section></div>`;
    document.querySelectorAll('[data-history-id]').forEach((node) => {
      const item = currentItems.find((entry) => entry.capture_id === node.dataset.historyId);
      node.addEventListener('click', () => selectItem(item, node));
      node.addEventListener('keydown', (event) => { if (event.key === 'Enter' || event.key === ' ') { event.preventDefault(); selectItem(item, node); } });
    });
    document.querySelector('[data-history-prev]')?.addEventListener('click', () => loadHistory(Math.max(1, page - 1)));
    document.querySelector('[data-history-next]')?.addEventListener('click', () => loadHistory(page + 1));
  };

  async function loadHistory(page = 1) {
    const response = await fetch(`/api/v1/history?page=${page}&limit=5`, { cache: 'no-store' });
    if (!response.ok) { content.innerHTML = '<section class="card">读取历史失败，请刷新后重试。</section>'; return; }
    const data = await response.json();
    currentItems = data.items || [];
    if (!currentItems.some((item) => item.capture_id === selected)) selected = null;
    render(data.page, data.page_size, data.total);
  }

  // The original settings script calls history() from its nav and pager handlers.
  window.__historyPanel = loadHistory;
  document.addEventListener('keydown', (event) => { if (event.key !== 'Escape') return; if (document.querySelector('[data-history-lightbox]')) closeLightbox(); else if (document.querySelector('[data-history-modal]')) closeModal(); });
})();
