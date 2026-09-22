(() => {
  const button = document.querySelector('.nav button[data-p="users"]');
  if (!button || window.__userPanelBound) return;
  window.__userPanelBound = true;
  const content = document.querySelector('#content');
  const title = document.querySelector('#title');
  const esc = (value) => String(value ?? '').replace(/[&<>"']/g, (c) => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
  let page = 1;
  let data = {items: [], page: 1, page_size: 5, total: 0};

  const render = () => {
    const totalPages = Math.max(1, Math.ceil(data.total / data.page_size));
    content.innerHTML = `
      <section class="card form user-panel">
        <h3>创建用户</h3>
        <p class="meta" style="margin-top:0">普通用户不能自行注册，由管理员统一创建。</p>
        <label>用户名</label><input id="managed-user-name" placeholder="请输入用户名">
        <label>密码</label><input id="managed-user-password" type="password" placeholder="至少 8 位">
        <div><button class="primary" id="managed-user-create">创建用户</button></div>
        <div id="managed-user-message"></div>
      </section>
      <section class="card list user-panel">
        <h3>用户列表</h3>
        ${data.items.length ? data.items.map((user) => `
          <article>
            <span style="flex:1"><b>${esc(user.username)}</b><br><small>${user.role === 'admin' ? '管理员' : '普通用户'}</small></span>
            ${user.role === 'user' ? `<button class="secondary" data-delete-user="${esc(user.id)}" data-user-name="${esc(user.username)}">删除用户</button>` : '<small class="meta">当前管理员</small>'}
          </article>`).join('') : '<p class="meta">暂无用户</p>'}
        <div class="pager">
          <button id="user-prev" ${page <= 1 ? 'disabled' : ''}>上一页</button>
          <span>第 ${page} 页 / ${totalPages}</span>
          <button id="user-next" ${page >= totalPages ? 'disabled' : ''}>下一页</button>
        </div>
      </section>`;
    document.querySelector('#user-prev')?.addEventListener('click', () => { page -= 1; load(); });
    document.querySelector('#user-next')?.addEventListener('click', () => { page += 1; load(); });
    document.querySelector('#managed-user-create')?.addEventListener('click', async () => {
      const username = document.querySelector('#managed-user-name').value.trim();
      const password = document.querySelector('#managed-user-password').value;
      const message = document.querySelector('#managed-user-message');
      const result = await fetch('/api/v1/admin/users', {method:'POST', headers:{'content-type':'application/json'}, body:JSON.stringify({username, password})});
      if (result.ok) {
        page = 1;
        await load();
        document.querySelector('#managed-user-message').innerHTML = `<p style="color:#15966d">用户已创建，密码：<code>${esc(password)}</code>（请妥善保存）</p>`;
      } else message.innerHTML = '<p style="color:#dc3b4b">创建失败：用户名不能为空且密码至少 8 位。</p>';
    });
    document.querySelectorAll('[data-delete-user]').forEach((remove) => remove.addEventListener('click', async () => {
      if (!confirm(`确认删除用户“${remove.dataset.userName}”？`)) return;
      const result = await fetch('/api/v1/admin/users/' + encodeURIComponent(remove.dataset.deleteUser), {method:'DELETE'});
      if (result.ok) await load();
      else alert(result.status === 403 ? '不能删除管理员' : '删除失败');
    }));
  };
  const load = async () => {
    const result = await fetch('/api/v1/admin/users?page=' + page + '&limit=5', {cache:'no-store'});
    if (!result.ok) { content.innerHTML = '<section class="card">仅管理员可访问用户管理。</section>'; return; }
    data = await result.json();
    page = data.page;
    render();
  };
  button.onclick = null;
  button.addEventListener('click', () => {
    document.querySelectorAll('.nav button').forEach((item) => item.classList.toggle('active', item === button));
    title.textContent = '用户管理';
    page = 1;
    load();
  });
})();
