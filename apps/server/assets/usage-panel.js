(() => {
  const button = document.querySelector('.nav button[data-p="usage"]');
  if (!button || window.__usagePanelBound) return;
  window.__usagePanelBound = true;
  const content = document.querySelector('#content');
  const title = document.querySelector('#title');
  const esc = (value) => String(value ?? '').replace(/[&<>"']/g, (c) => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
  const fmt = (value) => value ? new Date(value).toLocaleString('zh-CN', {hour12:false}) : '暂无';
  const style = document.createElement('style');
  style.textContent = `
    .usage-summary{display:grid;grid-template-columns:repeat(4,minmax(0,1fr));gap:14px;margin-bottom:18px}
    .usage-metric{padding:18px 20px;background:linear-gradient(145deg,#fff,#f5f8ff);border:1px solid var(--line);border-radius:15px}
    .usage-metric small{display:block;color:var(--muted);margin-bottom:8px}.usage-metric strong{font-size:28px;letter-spacing:-.03em}
    .usage-trend{display:flex;align-items:end;gap:10px;height:180px;padding:18px 10px 28px;border-bottom:1px solid var(--line)}
    .usage-bar{display:flex;flex:1;min-width:24px;height:100%;flex-direction:column;justify-content:end;align-items:center;gap:7px}.usage-bar i{display:block;width:min(34px,70%);min-height:3px;border-radius:7px 7px 2px 2px;background:linear-gradient(180deg,#6b96ff,#3f7cff)}.usage-bar small{color:var(--muted);font-size:11px;white-space:nowrap;transform:rotate(-35deg);transform-origin:top center}.usage-bar b{font-size:11px;color:#3f5d9c}
    .usage-table{width:100%;border-collapse:collapse}.usage-table th,.usage-table td{padding:12px 10px;border-bottom:1px solid var(--line);text-align:left}.usage-table th{font-size:12px;color:var(--muted);font-weight:600}.usage-table td:not(:first-child),.usage-table th:not(:first-child){text-align:right}.usage-table tr:last-child td{border-bottom:0}.usage-empty{padding:28px;text-align:center;color:var(--muted)}
    @media(max-width:900px){.usage-summary{grid-template-columns:repeat(2,minmax(0,1fr))}.usage-table{min-width:680px}.usage-table-wrap{overflow:auto}}
  `;
  document.head.appendChild(style);

  const render = (report) => {
    const maxDaily = Math.max(1, ...report.daily.map((item) => item.calls));
    const bars = report.daily.length ? report.daily.map((item) => `<div class="usage-bar" title="${esc(item.date)}：${item.calls} 次"><b>${item.calls}</b><i style="height:${Math.max(3, Math.round(item.calls / maxDaily * 100))}%"></i><small>${esc(item.date.slice(5))}</small></div>`).join('') : '<div class="usage-empty">近 14 天暂无解析调用</div>';
    const rows = report.users.length ? report.users.map((user) => `<tr><td><b>${esc(user.username)}</b></td><td>${user.total_calls}</td><td>${user.completed_calls}</td><td>${user.failed_calls}</td><td>${user.active_calls}</td><td>${fmt(user.last_called_at)}</td></tr>`).join('') : '<tr><td colspan="6" class="usage-empty">暂无用户调用记录</td></tr>';
    content.innerHTML = `<div class="usage-summary"><div class="usage-metric"><small>总解析调用</small><strong>${report.total_calls}</strong></div><div class="usage-metric"><small>已完成</small><strong>${report.completed_calls}</strong></div><div class="usage-metric"><small>失败/超时</small><strong>${report.failed_calls}</strong></div><div class="usage-metric"><small>进行中</small><strong>${report.active_calls}</strong></div></div><section class="card"><h3>近 14 天调用趋势</h3><div class="usage-trend" aria-label="近 14 天调用次数">${bars}</div></section><section class="card"><h3>用户调用明细</h3><div class="usage-table-wrap"><table class="usage-table"><thead><tr><th>用户</th><th>总次数</th><th>已完成</th><th>失败/超时</th><th>进行中</th><th>最近调用</th></tr></thead><tbody>${rows}</tbody></table></div></section>`;
  };

  window.usage = async () => {
    title.textContent = '使用记录';
    content.innerHTML = '<section class="card">正在加载使用记录…</section>';
    try {
      const response = await fetch('/api/v1/admin/usage', {cache:'no-store'});
      if (!response.ok) throw new Error('usage unavailable');
      render(await response.json());
    } catch (_) {
      content.innerHTML = '<section class="card">使用记录读取失败，请刷新后重试。</section>';
    }
  };

  button.onclick = null;
  button.addEventListener('click', () => {
    document.querySelectorAll('.nav button').forEach((item) => item.classList.toggle('active', item === button));
    page = 'usage';
    pageNo = 1;
    window.usage();
  });
})();
