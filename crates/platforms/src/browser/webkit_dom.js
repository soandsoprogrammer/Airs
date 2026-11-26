(action, arguments) => {
  const registry = globalThis.__airsDomRefs ?? Object.defineProperty(globalThis, '__airsDomRefs', {
    configurable: false, value: new Map()
  }).__airsDomRefs;
  const id = node => {
    for (const [key, value] of registry) if (value === node) return key;
    let key;
    do { key = crypto.getRandomValues(new Uint32Array(1))[0] || 1; } while (registry.has(key));
    registry.set(key, node);
    return key;
  };
  const lookup = value => {
    const node = registry.get(Number(value));
    return node?.isConnected ? node : null;
  };
  const describe = (node, depth) => {
    const key = id(node);
    const info = {
      nodeId: key, backendNodeId: key, nodeType: node.nodeType,
      nodeName: node.nodeName, localName: node.localName ?? '', nodeValue: node.nodeValue ?? '',
      childNodeCount: node.childNodes.length,
    };
    if (node.attributes) info.attributes = [...node.attributes].flatMap(a => [a.name, a.value]);
    if (depth > 0) {
      info.children = [...node.childNodes].map(child => describe(child, depth - 1));
      const shadow = node.shadowRoot ?? globalThis.__airsTrackedShadow?.(node);
      if (shadow) info.shadowRoots = [describe(shadow, depth - 1)];
    }
    return info;
  };
  const remote = value => {
    if (value && typeof value.nodeType === "number" && value.ownerDocument && value.isConnected) {
      return {type:'object', subtype:'node', objectId:String(id(value))};
    }
    return {type:'object', value: typeof value === "function" || (value && typeof value.nodeType === "number") ? null : (value ?? null)};
  };
  switch (action) {
    case 'evaluate': return Promise.resolve((0,eval)(arguments.expression)).then(remote);
    case 'describe': {
      const node = lookup(arguments.objectId ?? arguments.backendNodeId ?? arguments.nodeId);
      return node ? {node:describe(node, Math.max(0, Math.min(arguments.depth ?? 0, 8)))} : null;
    }
    case 'resolve': {
      const node = lookup(arguments.backendNodeId ?? arguments.nodeId);
      return node ? {object:remote(node)} : null;
    }
    case 'call': {
      const node = lookup(arguments.objectId);
      if (!node) return null;
      const f = (0,eval)('(' + arguments.functionDeclaration + ')');
      return Promise.resolve(f.call(node, ...(arguments.arguments ?? []).map(a => a.value)))
        .then(value => ({result:remote(value)}));
    }
    case 'release': registry.delete(Number(arguments.objectId)); return {};
    default: return null;
  }
}
