const header = document.querySelector('[data-header]');
const menuButton = document.querySelector('[data-menu-button]');

const RELEASE_API = 'https://api.github.com/repos/impala84/anamorphic-proxy/releases/latest';

async function applyLatestRelease() {
  try {
    const response = await fetch(RELEASE_API, {
      headers: { Accept: 'application/vnd.github+json' }
    });
    if (!response.ok) throw new Error(`GitHub release lookup failed (${response.status})`);

    const release = await response.json();
    const version = String(release.tag_name || '').replace(/^v/, '');
    const dmg = release.assets?.find((asset) => /Apple-Silicon\.dmg$/i.test(asset.name));
    if (!version || !dmg?.browser_download_url) return;

    document.querySelectorAll('[data-latest-download]').forEach((link) => {
      link.href = dmg.browser_download_url;
    });
    document.querySelectorAll('[data-latest-version]').forEach((label) => {
      label.textContent = label.textContent.trim().startsWith('v') ? `v${version}` : version;
    });

    const schema = document.querySelector('script[type="application/ld+json"]');
    if (schema) {
      const metadata = JSON.parse(schema.textContent);
      metadata.softwareVersion = version;
      metadata.downloadUrl = dmg.browser_download_url;
      schema.textContent = JSON.stringify(metadata);
    }
  } catch (error) {
    console.info('Using the bundled DeProxy release link.', error);
  }
}

applyLatestRelease();

window.addEventListener('scroll', () => {
  header.classList.toggle('scrolled', window.scrollY > 4);
}, { passive: true });

menuButton.addEventListener('click', () => {
  const open = header.classList.toggle('menu-open');
  menuButton.setAttribute('aria-expanded', String(open));
});

header.querySelectorAll('nav a').forEach((link) => link.addEventListener('click', () => {
  header.classList.remove('menu-open');
  menuButton.setAttribute('aria-expanded', 'false');
}));

const observer = new IntersectionObserver((entries) => {
  entries.forEach((entry) => {
    if (entry.isIntersecting) {
      entry.target.classList.add('visible');
      observer.unobserve(entry.target);
    }
  });
}, { threshold: 0.12, rootMargin: '0px 0px -5% 0px' });

document.querySelectorAll('.reveal').forEach((element) => observer.observe(element));

document.querySelectorAll('[data-tabs]').forEach((tabs) => {
  const buttons = [...tabs.querySelectorAll('[data-tab]')];
  const panels = [...tabs.querySelectorAll('[data-panel]')];
  buttons.forEach((button) => button.addEventListener('click', () => {
    const target = button.dataset.tab;
    buttons.forEach((item) => item.setAttribute('aria-selected', String(item === button)));
    panels.forEach((panel) => {
      const active = panel.dataset.panel === target;
      panel.hidden = !active;
      panel.classList.toggle('active', active);
    });
  }));
});
