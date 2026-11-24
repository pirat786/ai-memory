const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

let currentProjectId = 'all';
let searchTimer = null;
let isGoogleConnected = false;
let lang = localStorage.getItem('aimem_lang') || 'ru';

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
  document.getElementById('cfgAutostartLabel').textContent = t('autostart');
  document.getElementById('reauthBtn').textContent = t('reauth');
  document.getElementById('closeModalBtn').textContent = t('close');
  document.getElementById('saveCfgBtn').textContent = t('save');

  const kindSel = document.getElementById('newKind');
  const prevKind = kindSel.value;
  kindSel.innerHTML = t('kinds')
    .map(([v, lbl]) => `<option value="${v}">${lbl}</option>`)
    .join('');
  kindSel.value = prevKind || 'decision';
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
  document.getElementById('cfgDevice').value = st.device_name || '';
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
    loadMemories();
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
      loadMemories();
    };
    list.appendChild(el);
  }
}

function debouncedLoadMemories() {
  clearTimeout(searchTimer);
  searchTimer = setTimeout(loadMemories, 180);
}

async function loadMemories() {
  const q = document.getElementById('searchInput').value.trim();
  const items = await invoke('list_memories', {
    projectId: currentProjectId === 'all' ? null : currentProjectId,
    query: q || null,
  });
  const box = document.getElementById('memoriesList');
  box.innerHTML = '';

  if (!items.length) {
    box.innerHTML = `<div style="color:var(--muted);font-size:13px;padding:16px;">${escapeHtml(t('noMemories'))}</div>`;
    return;
  }

  for (const m of items) {
    const card = document.createElement('div');
    card.className = 'memory-card';
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
          <button class="danger" style="padding:3px 8px;font-size:11px;" onclick="deleteMemory('${m.id}')">${escapeHtml(t('delete'))}</button>
        </div>
      </div>
      <div class="memory-body">${escapeHtml(m.content)}</div>
    `;
    box.appendChild(card);
  }
}

async function addMemory() {
  const content = document.getElementById('newContent').value.trim();
  if (!content) return toast(t('enterText'));
  const projectId = currentProjectId === 'all' ? 'global' : currentProjectId;
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
}

async function deleteMemory(id) {
  await invoke('delete_memory', { id });
  toast(t('deleted'));
  await loadProjects();
  await loadMemories();
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
  await invoke('save_settings', {
    deviceName: document.getElementById('cfgDevice').value,
    googleClientId: null,
    googleClientSecret: null,
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
