/**
 * expressionUpdates.js
 * Utility to find and replace node label references in workflow expressions.
 * Ported/Inspired by n8n's applyAccessPatterns logic.
 */

function backslashEscape(nodeName) {
    const BACKSLASH_ESCAPABLE_CHARS = /[.*+?^${}()|[\]\\]/g;
    return nodeName.replace(BACKSLASH_ESCAPABLE_CHARS, (char) => `\\${char}`);
}

function dollarEscape(nodeName) {
    return nodeName.replace(new RegExp('\\$', 'g'), '$$$$');
}

function prepareOldNodeName(nodeName) {
    // if node name contains literal \ -> replace with \\
    const doubleSlashes = nodeName.replaceAll('\\', '\\\\');
    // escape special characters for regex
    const escaped = backslashEscape(doubleSlashes);
    // quotes may or may not be escaped in the JS expression
    return escaped.replace(/"/g, '(?:\\\\?")').replace(/'/g, "(?:\\\\?')");
}

function prepareNewNodeName(nodeName) {
    // escape $ for replacement regex
    const dollarEscaped = dollarEscape(nodeName);
    // escape literal \ ' " characters
    return dollarEscaped.replaceAll('\\', '\\\\').replaceAll('"', '\\"').replaceAll("'", "\\'");
}

const ACCESS_PATTERNS = [
    {
        checkPattern: '$(',
        replacePattern: (s) => `(\\$\\(['"])${s}(['"]\\))`,
    },
    {
        checkPattern: '$node[',
        replacePattern: (s) => `(\\$node\\[['"])${s}(['"]\\])`,
    },
    {
        checkPattern: 'node[', // also support without $
        replacePattern: (s) => `(node\\[['"])${s}(['"]\\])`,
    },
    {
        checkPattern: '$node.',
        replacePattern: (s) => `(\\$node\\.)${s}(\\.|\\s|\\}\\})`,
    },
    {
        checkPattern: 'node.', // also support without $
        replacePattern: (s) => `(node\\.)${s}(\\.|\\s|\\}\\})`,
    }
];

export function applyAccessPatterns(expression, previousName, newName) {
    let noMatch = true;
    for (const pattern of ACCESS_PATTERNS) {
        if (expression.includes(pattern.checkPattern)) {
            noMatch = false;
            break;
        }
    }

    if (noMatch) {
        return expression;
    }

    const preparedOldName = prepareOldNodeName(previousName);
    const preparedNewName = prepareNewNodeName(newName);

    let updatedExpression = expression;
    for (const pattern of ACCESS_PATTERNS) {
        if (updatedExpression.includes(pattern.checkPattern)) {
            const regex = new RegExp(pattern.replacePattern(preparedOldName), 'g');
            updatedExpression = updatedExpression.replace(regex, `$1${preparedNewName}$2`);
        }
    }
    return updatedExpression;
}

export function renameNodeInExpressions(nodes, oldLabel, newLabel) {
    if (!nodes || !oldLabel || !newLabel || oldLabel === newLabel) return nodes;

    nodes.forEach(node => {
        if (node.data && node.data.config) {
            updateValueRecursively(node.data.config, (val) => {
                if (typeof val === 'string') {
                    return applyAccessPatterns(val, oldLabel, newLabel);
                }
                return val;
            });
        }
    });

    return nodes;
}

function updateValueRecursively(obj, updater) {
    if (!obj) return;

    if (Array.isArray(obj)) {
        obj.forEach((item, index) => {
            if (typeof item === 'object' && item !== null) {
                updateValueRecursively(item, updater);
            } else {
                obj[index] = updater(item);
            }
        });
    } else if (typeof obj === 'object' && obj !== null) {
        Object.keys(obj).forEach(key => {
            const value = obj[key];
            if (typeof value === 'object' && value !== null) {
                updateValueRecursively(value, updater);
            } else {
                obj[key] = updater(value);
            }
        });
    }
}
