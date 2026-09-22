(() => {
  const button = document.querySelector('.nav button[data-p="devices"]');
  if (!button || window.__capturePanelBound) return;
  window.__capturePanelBound = true;
  const content = document.querySelector('#content');
  const title = document.querySelector('#title');
  const esc = (value) => String(value ?? '').replace(/[&<>"']/g, (c) => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
  let devices = [];
  let devicePage = 1;
  let loadGeneration = 0;
  const pageSize = 5;

  const tokenMessage = (label, token) => `
    <div class="meta" style="margin-top:12px;align-items:center;gap:8px">
      <span>${esc(label)}：<code style="word-break:break-all">${esc(token)}</code></span>
      <button class="secondary" data-copy-token="${esc(token)}">复制</button>
    </div>`;

  const copyText = async (value) => {
    // 不依赖 isSecureContext：部分桌面 WebView 会错误地返回 false，
    // 但 navigator.clipboard 实际可用。先尝试原生 API，失败再降级。
    if (navigator.clipboard?.writeText) {
      try {
        await navigator.clipboard.writeText(value);
        return;
      } catch (_) {
        // 继续使用 textarea + execCommand 兼容方案。
      }
    }
    const textarea = document.createElement('textarea');
    textarea.value = value;
    textarea.setAttribute('readonly', '');
    textarea.style.position = 'fixed';
    textarea.style.opacity = '0';
    document.body.appendChild(textarea);
    textarea.focus();
    textarea.select();
    const copied = document.execCommand('copy');
    textarea.remove();
    if (!copied) throw new Error('clipboard copy failed');
  };

  const copyToken = async (copy) => {
    if (!copy || copy.disabled) return;
    const original = copy.textContent;
    copy.disabled = true;
    copy.textContent = '复制中…';
    copy.setAttribute('aria-live', 'polite');
    try {
      await copyText(copy.dataset.copyToken || '');
      copy.textContent = '已复制 ✓';
      copy.style.color = '#15966d';
      copy.style.borderColor = '#9ed9bf';
    } catch (_) {
      copy.textContent = '复制失败';
      copy.style.color = '#dc3b4b';
      copy.style.borderColor = '#efb6bd';
    } finally {
      setTimeout(() => {
        copy.disabled = false;
        copy.textContent = original;
        copy.style.color = '';
        copy.style.borderColor = '';
      }, 1600);
    }
  };

  // 使用捕获阶段事件代理，避免按钮由 innerHTML 动态生成后没有绑定事件，
  // 也避免设置页其它菜单脚本覆盖复制按钮的点击行为。
  document.addEventListener('click', (event) => {
    const copy = event.target.closest?.('[data-copy-token]');
    if (!copy) return;
    event.preventDefault();
    event.stopImmediatePropagation();
    copyToken(copy);
  }, true);

  const render = () => {
    const totalPages = Math.max(1, Math.ceil(devices.length / pageSize));
    devicePage = Math.min(devicePage, totalPages);
    const rows = devices.slice((devicePage - 1) * pageSize, devicePage * pageSize);
    content.innerHTML = `
      <section class="card form capture-panel">
        <h3>绑定 Capture</h3>
        <p class="meta" style="margin-top:0">每台设备使用独立 Token，生成后只显示一次。</p>
        <label>设备 ID</label><input id="capture-device-id" placeholder="例如 mac-m5">
        <label>设备名称（可选）</label><input id="capture-device-name" placeholder="例如 我的 Mac">
        <div><button class="primary" id="capture-bind">生成设备 Token</button></div>
        <div id="capture-token"></div>
      </section>
      <section class="card list capture-panel">
        <h3>设备列表</h3>
        ${rows.length ? rows.map((device) => `
          <article>
            <span style="flex:1">
              <b>${esc(device.name || device.device_id)}</b><br>
              <small>${esc(device.device_id)} · <span style="color:${device.connected ? '#15966d' : '#dc3b4b'}">● ${device.connected ? '在线' : '离线'}</span> · 心跳 ${device.heartbeat_count || 0} 次</small>
            </span>
            <span style="display:flex;gap:8px;flex-wrap:wrap;justify-content:flex-end">
              <button class="secondary" data-rotate="${esc(device.id)}">重新生成 Token</button>
              <button class="secondary" data-delete="${esc(device.id)}">删除</button>
            </span>
          </article>`).join('') : '<p class="meta">暂无已绑定设备</p>'}
        <div class="pager">
          <button id="capture-prev" ${devicePage <= 1 ? 'disabled' : ''}>上一页</button>
          <span>第 ${devicePage} 页 / ${totalPages}</span>
          <button id="capture-next" ${devicePage >= totalPages ? 'disabled' : ''}>下一页</button>
        </div>
      </section>`;

    document.querySelector('#capture-prev')?.addEventListener('click', () => { devicePage -= 1; render(); });
    document.querySelector('#capture-next')?.addEventListener('click', () => { devicePage += 1; render(); });
    document.querySelector('#capture-bind')?.addEventListener('click', async () => {
      const deviceId = document.querySelector('#capture-device-id').value.trim();
      const name = document.querySelector('#capture-device-name').value.trim();
      const result = await fetch('/api/v1/devices', {method:'POST', headers:{'content-type':'application/json'}, body:JSON.stringify({device_id:deviceId, name})});
      const output = document.querySelector('#capture-token');
      if (result.ok) { const data = await result.json(); await load(); const freshOutput = document.querySelector('#capture-token'); if (freshOutput) freshOutput.innerHTML = tokenMessage('设备 Token（仅显示一次）', data.token); }
      else output.innerHTML = `<p style="color:#dc3b4b">${result.status === 409 ? '该设备 ID 已被绑定' : '绑定失败，请检查输入'}</p>`;
    });
    document.querySelectorAll('[data-rotate]').forEach((rotate) => rotate.addEventListener('click', async () => {
      rotate.disabled = true;
      const original = rotate.textContent;
      rotate.textContent = '生成中…';
      const result = await fetch('/api/v1/devices/' + encodeURIComponent(rotate.dataset.rotate) + '/rotate-token', {method:'POST'});
      if (result.ok) { const data = await result.json(); const output = document.querySelector('#capture-token'); if (output) output.innerHTML = tokenMessage('新 Token（仅显示一次）', data.token); }
      else { const output = document.querySelector('#capture-token'); if (output) output.innerHTML = `<p style="color:#dc3b4b">Token 生成失败（HTTP ${result.status}）</p>`; }
      rotate.disabled = false;
      rotate.textContent = original;
    }));
    document.querySelectorAll('[data-delete]').forEach((remove) => remove.addEventListener('click', async () => {
      if (!confirm('确认删除该设备？删除后原 Token 立即失效。')) return;
      const result = await fetch('/api/v1/devices/' + encodeURIComponent(remove.dataset.delete), {method:'DELETE'});
      if (result.ok) await load();
    }));
  };
  const load = async () => {
    const generation = ++loadGeneration;
    try {
      const result = await fetch('/api/v1/devices', {cache:'no-store'});
      if (page !== 'devices' || generation !== loadGeneration) return;
      if (!result.ok) throw new Error(`device list request failed: ${result.status}`);
      devices = await result.json();
      render();
    } catch (_) {
      if (page !== 'devices' || generation !== loadGeneration) return;
      content.innerHTML = '<section class="card">设备列表读取失败，请刷新后重试。</section>';
    }
  };
  // settings_fixed.html 的通用菜单脚本只认识三个原有页面；Capture 使用自己的处理器，避免触发旧的兜底分支。
  button.onclick = null;
  button.addEventListener('click', () => {
    page = 'devices';
    document.querySelectorAll('.nav button').forEach((item) => item.classList.toggle('active', item === button));
    title.textContent = 'Capture 设备';
    devicePage = 1;
    load();
  });
})();
