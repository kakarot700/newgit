(() => {
  const menu = document.querySelector('.menu-toggle');
  const nav = document.querySelector('.nav-links');
  menu?.addEventListener('click', () => {
    const open = menu.getAttribute('aria-expanded') !== 'true';
    menu.setAttribute('aria-expanded', String(open));
    menu.setAttribute('aria-label', open ? 'Close navigation' : 'Open navigation');
    nav?.classList.toggle('is-open', open);
  });
  nav?.querySelectorAll('a').forEach(link => link.addEventListener('click', () => {
    nav.classList.remove('is-open');
    menu?.setAttribute('aria-expanded', 'false');
    menu?.setAttribute('aria-label', 'Open navigation');
  }));

  const copyButton = document.querySelector('.copy-button');
  copyButton?.addEventListener('click', async () => {
    const text = [...document.querySelectorAll('.terminal-line')].map(line => line.textContent.replace(/^\$\s*/, '').trim()).join('\n');
    try {
      await navigator.clipboard.writeText(text);
      copyButton.innerHTML = 'COPIED <span>✓</span>';
      window.setTimeout(() => { copyButton.innerHTML = 'COPY <span>↗</span>'; }, 1600);
    } catch {
      copyButton.textContent = 'SELECT COMMANDS';
    }
  });

  const modelNodes = [...document.querySelectorAll('.model-node')];
  const modelCaption = document.getElementById('model-caption-text');
  modelNodes.forEach(node => node.addEventListener('mouseenter', () => {
    const name = node.firstChild.textContent.trim().toLowerCase();
    const related = node.dataset.links.split(' ');
    modelNodes.forEach(item => item.classList.toggle('related', related.includes(item.firstChild.textContent.trim().toLowerCase())));
    node.classList.add('active');
    if (modelCaption) modelCaption.textContent = `${node.firstChild.textContent.trim()} connects to ${related.join(', ')}.`;
  }));
  document.querySelector('.model-visual')?.addEventListener('mouseleave', () => {
    modelNodes.forEach(item => item.classList.remove('related'));
    if (modelCaption) modelCaption.textContent = 'Related concepts illuminate one another. Select any node.';
  });
  modelNodes.forEach(node => node.addEventListener('focus', () => node.dispatchEvent(new Event('mouseenter'))));

  const events = [
    ['A developer defines the goal.', 'Intent is captured before code changes begin, giving people and agents a shared target.', 'goal.create · "Improve authentication reliability"', 'GOAL · OPEN'],
    ['An agent investigates the repository.', 'The agent works from an explicit goal and isolated workspace; repository content is not implicitly executed.', 'workspace.create · agent-session', 'WORKSPACE · ACTIVE'],
    ['A candidate change is proposed.', 'A change links a candidate result to its base snapshot and the goal it aims to achieve.', 'change create "Refresh session flow" --base <snapshot> --result <snapshot> --goal <goal-id>', 'CHANGE · PROPOSED'],
    ['A requested check becomes evidence.', 'Only the command explicitly supplied to the evidence operation is run; its result is recorded against the change.', 'evidence record --kind unit_test --target <change-id> -w agent-session -- cargo test', 'EVIDENCE · RECORDED'],
    ['Evaluation adds a distinct review.', 'Evidence remains distinct from evaluation; deterministic aggregation is available, while AI-generated opinions are labeled.', 'evaluation from-evidence <change-id>', 'EVALUATION · RECORDED'],
    ['A human approves a proposal.', 'The proposal bundles rationale, change, evidence, and an explicit approval before integration.', 'proposal approve <proposal-id> --author human:reviewer', 'APPROVAL · HUMAN'],
    ['The selected proposal is integrated.', 'After approval, integration applies the selected proposal through NewGit’s transactional repository operations.', 'proposal integrate <proposal-id>', 'INTEGRATION · APPLIED'],
    ['The record stays connected.', 'The goal, change, evidence, proposal, decision, and updated snapshot remain linked in repository history.', 'goal set-status <goal-id> achieved', 'HISTORY · VERSIONED']
  ];
  let step = 0;
  const setStep = next => {
    step = Math.max(0, Math.min(events.length - 1, next));
    const [title, copy, code, state] = events[step];
    document.getElementById('demo-event-index').textContent = `EVENT ${String(step + 1).padStart(2, '0')} / 08`;
    document.getElementById('demo-event-title').textContent = title;
    document.getElementById('demo-event-copy').textContent = copy;
    document.getElementById('demo-event-code').textContent = code;
    document.getElementById('demo-state').textContent = state;
    document.getElementById('demo-prev').disabled = step === 0;
    document.getElementById('demo-next').disabled = step === events.length - 1;
    document.getElementById('demo-next').textContent = step === events.length - 1 ? 'Workflow complete ✓' : 'Next event →';
    document.getElementById('demo-progress-fill').style.width = `${(step / (events.length - 1)) * 100}%`;
    document.querySelectorAll('.demo-step').forEach((button, index) => {
      button.classList.toggle('active', index === step);
      button.classList.toggle('complete', index < step);
      if (index === step) button.setAttribute('aria-current', 'step'); else button.removeAttribute('aria-current');
    });
  };
  document.getElementById('demo-prev')?.addEventListener('click', () => setStep(step - 1));
  document.getElementById('demo-next')?.addEventListener('click', () => setStep(step + 1));
  document.querySelectorAll('.demo-step').forEach(button => button.addEventListener('click', () => setStep(Number(button.dataset.step))));

  if ('IntersectionObserver' in window && !window.matchMedia('(prefers-reduced-motion: reduce)').matches) {
    const observer = new IntersectionObserver(entries => entries.forEach(entry => {
      if (entry.isIntersecting) { entry.target.classList.add('is-visible'); observer.unobserve(entry.target); }
    }), { threshold: 0.08 });
    document.querySelectorAll('.step, .principle, .engineering-grid a, .model-visual, .agent-flow, .compat-diagram').forEach(el => { el.classList.add('reveal'); observer.observe(el); });
  }
})();
