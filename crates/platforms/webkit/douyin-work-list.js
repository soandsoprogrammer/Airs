// Match a newly published video from the official creator work-list response.
// The only data leaving the page is the matched work ID and creation timestamp.
(criteria, payload) => {
  if (!payload || (payload.status_code !== undefined && payload.status_code !== 0) ||
      !Array.isArray(payload.aweme_list)) return null;
  const normalize = value => typeof value === 'string' ? value.replace(/\s+/g, ' ').trim() : null;
  const title = normalize(criteria.title);
  const description = normalize(criteria.description);
  const fullCaption = normalize(title + ' ' + description);
  const found = new Map();
  for (const work of payload.aweme_list) {
    if (!work || typeof work !== 'object' || work.aweme_type !== 4 ||
        work.status_value !== 102 || work.status?.is_delete !== false ||
        work.status?.is_prohibited !== false || work.status?.is_private !== false ||
        normalize(work.item_title) !== title ||
        normalize(work.desc) !== fullCaption ||
        typeof work.aweme_id !== 'string' || !/^[1-9]\d{10,24}$/.test(work.aweme_id) ||
        !Number.isSafeInteger(work.create_time) || work.create_time < criteria.since ||
        work.create_time > criteria.until) continue;
    found.set(work.aweme_id, { id: work.aweme_id, createdAt: work.create_time });
    if (found.size > 1) break;
  }
  return [...found.values()];
}
