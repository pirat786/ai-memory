const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

let currentProjectId = 'all';
let searchTimer = null;
let isGoogleConnected = false;
let lang = localStorage.getItem('aimem_lang') || 'ru';
const PAGE_SIZE = 100;
let currentOffset = 0;
const loadedMemoriesMap = new Map();

function t(key) {
  const dict = (window.AIMEM_I18N && window.AIMEM_I18N[lang]) || window.AIMEM_I18N.ru;
  return dict[key];
}

function toast(msg) {
  const el = document.getElementById('toast');
  el.textContent = msg;
  el.classList.add('show');
  setTimeout(() => el.classList.remove('show'), 3200);
}

function applyTranslations() {
  document.documentElement.lang = lang;
  document.getElementById('langBtn').textContent = t('langBtn');
  document.getElementById('agentsReadyText').textContent = t('agentsReady');
  document.getElementById('settingsBtn').textContent = t('settings');
  document.getElementById('googleLogoutBtn').textContent = t('signOut');
  document.getElementById('searchInput').placeholder = t('searchPh');
  document.getElementById('importBtn').textContent = t('importBtn');
  document.getElementById('newBtn').textContent = t('newBtn');
  document.getElementById('newContent').placeholder = t('contentPh');
  document.getElementById('newTags').placeholder = t('tagsPh');
  document.getElementById('saveMemBtn').textContent = t('save');
  document.getElementById('settingsTitle').textContent = t('settingsTitle');
  document.getElementById('cfgDeviceLabel').textContent = t('pcName');
  document.getElementById('cfgScanDirsLabel').textContent = t('scanDirsLabel');
  document.getElementById('cfgAutoSyncLabel').textContent = t('autoSyncLabel');
  document.getElementById('cfgAutostartLabel').textContent = t('autostart');
  document.getElementById('reauthBtn').textContent = t('reauth');
  document.getElementById('closeModalBtn').textContent = t('close');
  document.getElementById('saveCfgBtn').textContent = t('save');
  document.getElementById('loadMoreBtn').textContent = t('loadMore');

  const kindSel = document.getElementById('newKind');
  const prevKind = kindSel.value;
  kindSel.innerHTML = t('kinds')
    .map(([v, lbl]) => `<option value="${v}">${lbl}</option>`)
    .join('');
  kindSel.value = prevKind || 'decision';

  const filterKindSel = document.getElementById('filterKind');
  const prevFilterKind = filterKindSel.value || 'all';
  filterKindSel.innerHTML = `<option value="all">${escapeHtml(t('allKinds'))}</option>` +
    t('kinds').map(([v]) => `<option value="${v}">${v}</option>`).join('');
  filterKindSel.value = prevFilterKind;

  const filterAgentSel = document.getElementById('filterAgent');
  if (filterAgentSel.options.length > 0) {
    filterAgentSel.options[0].textContent = t('allAgents');
  }
}

async function toggleLang() {
  lang = lang === 'ru' ? 'en' : 'ru';
  localStorage.setItem('aimem_lang', lang);
  applyTranslations();
  await loadStatus();
  await loadProjects();
  await loadMemories();
}

async function loadStatus() {
  const st = await invoke('get_status');
  isGoogleConnected = !!st.google_connected;
  const dot = document.getElementById('syncDot');
  const txt = document.getElementById('syncStatusText');
  const btn = document.getElementById('googleActionBtn');
  const logoutBtn = document.getElementById('googleLogoutBtn');

  if (isGoogleConnected) {
    dot.classList.add('ok');
    txt.textContent = st.last_sync_at
      ? t('syncPrefix') + new Date(st.last_sync_at).toLocaleString()
      : t('googleConnected');
    btn.textContent = t('syncNow');
    btn.classList.remove('primary');
    logoutBtn.style.display = 'inline-flex';
  } else {
    dot.classList.remove('ok');
    txt.textContent = t('googleDisconnected');
    btn.textContent = t('connectGoogle');
    btn.classList.add('primary');
    logoutBtn.style.display = 'none';
  }
  if (st.version) {
    document.getElementById('appVersion').textContent = `v${st.version}`;
  }
  document.getElementById('cfgDevice').value = st.device_name || '';
  document.getElementById('cfgAutoSync').checked = st.auto_sync !== false;
  document.getElementById('cfgScanDirs').value = (st.scan_dirs || []).join('\n');
}

async function handleGoogleAction() {
  if (isGoogleConnected) {
    await syncNow();
  } else {
    await authGoogle();
  }
}

async function loadProjects() {
  const projects = await invoke('list_projects');
  const list = document.getElementById('projectList');
  list.innerHTML = '';

  const allEl = document.createElement('div');
  allEl.className = 'project-item' + (currentProjectId === 'all' ? ' active' : '');
  allEl.innerHTML = `<div><div class="project-name">${escapeHtml(t('allProjects'))}</div><div class="project-sub">${escapeHtml(t('allProjectsSub'))}</div></div>`;
  allEl.onclick = () => {
    currentProjectId = 'all';
    loadProjects();
    resetAndLoadMemories();
  };
  list.appendChild(allEl);

  for (const p of projects) {
    const el = document.createElement('div');
    el.className = 'project-item' + (currentProjectId === p.id ? ' active' : '');
    el.innerHTML = `
      <div style="min-width:0">
        <div class="project-name">${escapeHtml(p.name)}</div>
        <div class="project-sub">${escapeHtml(p.id)}</div>
      </div>
      <span class="badge">${p.count}</span>
    `;
    el.onclick = () => {
      currentProjectId = p.id;
      loadProjects();
      resetAndLoadMemories();
    };
    list.appendChild(el);
  }
}

function debouncedLoadMemories() {
  clearTimeout(searchTimer);
  searchTimer = setTimeout(resetAndLoadMemories, 180);
}

async function resetAndLoadMemories() {
  currentOffset = 0;
  await loadMemories(false);
}

async function loadMoreMemories() {
  currentOffset += PAGE_SIZE;
  await loadMemories(true);
}

function renderMemoryCard(m) {
  const card = document.createElement('div');
  card.className = 'memory-card';
  card.dataset.cardId = m.id;
  const tagsHtml = m.tags
    ? m.tags
        .split(',')
        .map((x) => x.trim())
        .filter(Boolean)
        .map((x) => `<span class="badge">#${escapeHtml(x)}</span>`)
        .join('')
    : '';
  card.innerHTML = `
    <div class="memory-meta">
      <div class="pills">
        <span class="pill kind-${escapeHtml(m.kind)}">${escapeHtml(m.kind)}</span>
        <span class="badge">${escapeHtml(m.project_id)}</span>
        <span class="badge">@${escapeHtml(m.source_agent)}</span>
        ${tagsHtml}
      </div>
      <div style="display:flex;align-items:center;gap:8px;">
        <span class="memory-time">${new Date(m.updated_at).toLocaleString()}</span>
        <button style="padding:3px 8px;font-size:11px;" data-edit-id="${escapeHtml(m.id)}">${escapeHtml(t('edit'))}</button>
        <button class="danger" style="padding:3px 8px;font-size:11px;" data-delete-id="${escapeHtml(m.id)}">${escapeHtml(t('delete'))}</button>
      </div>
    </div>
    <div class="memory-body">${escapeHtml(m.content)}</div>
  `;
  return card;
}

function openInlineEdit(id) {
  const m = loadedMemoriesMap.get(id);
  const card = document.querySelector(`[data-card-id="${CSS.escape(id)}"]`);
  if (!m || !card) return;
  const kindOptions = t('kinds')
    .map(([v]) => `<option value="${v}"${v === m.kind ? ' selected' : ''}>${v}</option>`)
    .join('');
  card.innerHTML = `
    <div style="display:flex;flex-direction:column;gap:8px;">
      <textarea class="edit-content" style="min-height:80px;">${escapeHtml(m.content)}</textarea>
      <div class="composer-row">
        <select class="edit-kind">${kindOptions}</select>
        <input class="edit-tags" value="${escapeHtml(m.tags || '')}" placeholder="${escapeHtml(t('tagsPh'))}" style="flex:1;" />
        <button data-cancel-edit="${escapeHtml(m.id)}">${escapeHtml(t('cancel'))}</button>
        <button class="primary" data-save-edit="${escapeHtml(m.id)}">${escapeHtml(t('save'))}</button>
      </div>
    </div>
  `;
}

async function saveInlineEdit(id) {
  const card = document.querySelector(`[data-card-id="${CSS.escape(id)}"]`);
  if (!card) return;
  const content = card.querySelector('.edit-content').value.trim();
  if (!content) return toast(t('enterText'));
  const kind = card.querySelector('.edit-kind').value;
  const tags = card.querySelector('.edit-tags').value;
  try {
    const updated = await invoke('update_memory', { id, kind, content, tags });
    if (updated) {
      loadedMemoriesMap.set(id, updated);
      card.replaceWith(renderMemoryCard(updated));
    } else {
      await resetAndLoadMemories();
    }
    toast(t('saved'));
  } catch (e) {
    toast(String(e));
  }
}

async function loadMemories(append = false) {
  const q = document.getElementById('searchInput').value.trim();
  const kind = document.getElementById('filterKind').value;
  const agent = document.getElementById('filterAgent').value;
  const items = await invoke('list_memories', {
    projectId: currentProjectId === 'all' ? null : currentProjectId,
    query: q || null,
    kind: kind === 'all' ? null : kind,
    agent: agent === 'all' ? null : agent,
    limit: PAGE_SIZE,
    offset: currentOffset,
  });
  const box = document.getElementById('memoriesList');
  if (!append) {
    box.innerHTML = '';
    loadedMemoriesMap.clear();
  }

  if (!append && !items.length) {
    box.innerHTML = `<div style="color:var(--muted);font-size:13px;padding:16px;">${escapeHtml(t('noMemories'))}</div>`;
    document.getElementById('loadMoreBtn').style.display = 'none';
    return;
  }

  for (const m of items) {
    loadedMemoriesMap.set(m.id, m);
    box.appendChild(renderMemoryCard(m));
  }
  document.getElementById('loadMoreBtn').style.display = items.length >= PAGE_SIZE ? 'inline-flex' : 'none';
}

async function addMemory() {
  const content = document.getElementById('newContent').value.trim();
  if (!content) return toast(t('enterText'));
  const projectId = currentProjectId === 'all' ? 'global' : currentProjectId;
  try {
    await invoke('add_memory', {
      projectId,
      kind: document.getElementById('newKind').value,
      content,
      tags: document.getElementById('newTags').value,
      sourceAgent: document.getElementById('newAgent').value,
    });
    document.getElementById('newContent').value = '';
    document.getElementById('newTags').value = '';
    toast(t('saved'));
    await loadProjects();
    await loadMemories();
  } catch (e) {
    toast(String(e));
  }
}

async function deleteMemory(id) {
  try {
    await invoke('delete_memory', { id });
    toast(t('deleted'));
    await loadProjects();
    await loadMemories();
  } catch (e) {
    toast(String(e));
  }
}

async function importMemories() {
  try {
    const res = await invoke('import_memories', { projectsDir: null });
    toast(`+${res.memories_imported} (${res.projects_touched} projects)`);
    await loadProjects();
    await loadMemories();
  } catch (e) {
    toast(String(e));
  }
}

async function syncNow() {
  const btn = document.getElementById('googleActionBtn');
  const prev = btn.textContent;
  btn.disabled = true;
  btn.textContent = t('syncing');
  try {
    const res = await invoke('sync_now');
    toast(`+${res.merged.memories_added} / ~${res.merged.memories_updated}`);
    await loadStatus();
    await loadProjects();
    await loadMemories();
  } catch (e) {
    toast(String(e));
  } finally {
    btn.disabled = false;
    if (btn.textContent === t('syncing')) btn.textContent = prev;
  }
}

async function openSettings() {
  document.getElementById('cfgAutostart').checked = await invoke('get_autostart');
  document.getElementById('settingsModal').classList.add('open');
}

function closeSettings() {
  document.getElementById('settingsModal').classList.remove('open');
}

async function disconnectGoogle() {
  await invoke('disconnect_google');
  toast(t('signedOut'));
  await loadStatus();
}

async function saveSettings() {
  const scanDirs = document
    .getElementById('cfgScanDirs')
    .value.split(/[\r\n,;]+/)
    .map((s) => s.trim())
    .filter(Boolean);
  await invoke('save_settings', {
    deviceName: document.getElementById('cfgDevice').value,
    googleClientId: null,
    googleClientSecret: null,
    autoSync: document.getElementById('cfgAutoSync').checked,
    scanDirs,
  });
  await invoke('set_autostart', {
    enabled: document.getElementById('cfgAutostart').checked,
  });
  toast(t('settingsSaved'));
  closeSettings();
  await loadStatus();
}

async function authGoogle() {
  const btn = document.getElementById('googleActionBtn');
  btn.disabled = true;
  btn.textContent = t('waitingBrowser');
  try {
    await invoke('auth_google');
    toast(t('googleOk'));
    closeSettings();
    await loadStatus();
    await syncNow();
  } catch (e) {
    toast(String(e));
    await loadStatus();
  } finally {
    btn.disabled = false;
  }
}

async function installUpdate() {
  const btn = document.getElementById('updateBtn');
  btn.disabled = true;
  btn.textContent = t('updating');
  try {
    await invoke('install_update');
  } catch (e) {
    toast(t('updateErr') + e);
    btn.disabled = false;
    btn.textContent = t('updateTo');
  }
}

function escapeHtml(str) {
  return String(str).replace(/[&<>"']/g, (c) => ({
    '&': '&amp;',
    '<': '&lt;',
    '>': '&gt;',
    '"': '&quot;',
    "'": '&#39;',
  }[c]));
}

(async () => {
  document.getElementById('memoriesList').addEventListener('click', (e) => {
    const delBtn = e.target.closest('[data-delete-id]');
    if (delBtn) return deleteMemory(delBtn.dataset.deleteId);
    const editBtn = e.target.closest('[data-edit-id]');
    if (editBtn) return openInlineEdit(editBtn.dataset.editId);
    const saveBtn = e.target.closest('[data-save-edit]');
    if (saveBtn) return saveInlineEdit(saveBtn.dataset.saveEdit);
    const cancelBtn = e.target.closest('[data-cancel-edit]');
    if (cancelBtn) {
      const m = loadedMemoriesMap.get(cancelBtn.dataset.cancelEdit);
      const card = document.querySelector(`[data-card-id="${CSS.escape(cancelBtn.dataset.cancelEdit)}"]`);
      if (m && card) card.replaceWith(renderMemoryCard(m));
    }
  });
  applyTranslations();
  await loadStatus();
  await loadProjects();
  await loadMemories();
  await listen('memory-synced', async () => {
    await loadStatus();
    await loadProjects();
    await loadMemories();
  });
  await listen('update-available', (e) => {
    const btn = document.getElementById('updateBtn');
    btn.textContent = t('updateTo') + e.payload;
    btn.style.display = 'inline-flex';
  });
})();
