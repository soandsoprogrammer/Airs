(editor, description, tags) => {
  const clean = (text) => text.replace(/\r\n?/g, '\n').replace(/\u200b/g, '')
    .replace(/[ \t\u00a0]+/g, ' ').trim();
  const topics = [...editor.querySelectorAll('[data-mention="#"]')].map((topic) =>
    clean(topic.innerText));
  if (topics.length !== tags.length ||
      topics.some((topic, index) => topic.toLowerCase() !== ('#' + tags[index]).toLowerCase())) {
    return 'topicsChanged';
  }
  let body = clean(editor.innerText);
  for (const topic of topics.reverse()) {
    if (!body.endsWith(topic)) return 'contentChanged';
    body = body.slice(0, -topic.length).trimEnd();
  }
  return body === clean(description) ? 'ready' : 'contentChanged';
}
