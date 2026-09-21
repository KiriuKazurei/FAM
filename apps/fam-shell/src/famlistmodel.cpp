#include "famlistmodel.h"

#include <QDateTime>
#include <QDir>

QString formatSize(quint64 bytes) {
    static const char *units[] = {"B", "K", "M", "G", "T"};
    if (bytes < 1024) {
        return QStringLiteral("%1 B").arg(bytes);
    }
    double value = static_cast<double>(bytes);
    int unit = 0;
    while (value >= 1024.0 && unit < 4) {
        value /= 1024.0;
        ++unit;
    }
    return QStringLiteral("%1 %2").arg(value, 0, 'f', 1).arg(QLatin1String(units[unit]));
}

FamListModel::FamListModel(QObject *parent) : QAbstractListModel(parent) {}

int FamListModel::rowCount(const QModelIndex &parent) const {
    return parent.isValid() ? 0 : int(m_total);
}

void FamListModel::bind(quint64 token, const QString &directory, const QStringList &kinds,
                        Fetcher fetch) {
    beginResetModel();
    m_token = token;
    m_directory = directory;
    m_kinds = kinds;
    m_fetch = std::move(fetch);
    m_total = 0;
    m_first = -1;
    m_rows.clear();
    endResetModel();
}

void FamListModel::loadStatic(const QVector<FamViewRow> &rows) {
    beginResetModel();
    m_token = 0;
    m_fetch = nullptr;
    m_directory.clear();
    m_rows = rows;
    m_first = 0;
    m_total = quint32(rows.size());
    endResetModel();
}

void FamListModel::unbind() {
    beginResetModel();
    m_token = 0;
    m_fetch = nullptr;
    m_total = 0;
    m_first = -1;
    m_rows.clear();
    endResetModel();
}

void FamListModel::readHead(const FamWindow &window) {
    if (window.state != FAM_READY && window.state != FAM_STALE) {
        return; /* loading or failed: total_rows is zero because nothing answered yet */
    }
    if (window.totalRows == m_total) {
        return;
    }
    beginResetModel();
    m_total = window.totalRows;
    m_first = -1;
    m_rows.clear();
    endResetModel();
}

/* Materialises the window covering `row`. Const because painting is what triggers it; the cache it
 * fills is mutable state that only ever holds rows the core has already served. */
bool FamListModel::fetchAt(int row) const {
    if (!m_fetch || m_total == 0) {
        return false;
    }
    const int count = qMin(m_viewport, int(m_total));
    const int lastStart = qMax(0, int(m_total) - count);
    const int first = qBound(0, row - count / 2, lastStart);

    FamWindow window;
    if (!m_fetch(quint32(first), quint32(count), &window)) {
        m_first = -1;
        m_rows.clear();
        return false;
    }
    m_first = first;
    m_rows = window.rows;
    if (window.totalRows != m_total) {
        /* The directory grew or shrank since the header we were painted from. Taking the new number
         * without model signals is safe here: nothing is mid-burst, and the next paint reads rows
         * out of the window this fetch just filled. */
        m_total = window.totalRows;
    }
    return true;
}

const FamViewRow *FamListModel::rowAt(int index) const {
    if (index < 0 || index >= int(m_total)) {
        return nullptr;
    }
    const bool outside = m_first < 0 || index < m_first || index >= m_first + m_rows.size();
    if (outside && !fetchAt(index)) {
        return nullptr;
    }
    const int offset = index - m_first;
    if (offset < 0 || offset >= m_rows.size()) {
        return nullptr;
    }
    return &m_rows.at(offset);
}

QVariant FamListModel::data(const QModelIndex &index, int role) const {
    if (!index.isValid()) {
        return {};
    }
    const FamViewRow *row = rowAt(index.row());
    if (row == nullptr) {
        return {};
    }
    switch (role) {
    case Qt::DisplayRole: {
        QString display = QStringLiteral("%1  %2").arg(row->name, formatSize(row->size));
        if (row->kind < m_kinds.size()) {
            display += QStringLiteral("  [%1]").arg(m_kinds.at(row->kind));
        }
        return display;
    }
    case Qt::ToolTipRole:
    case PathRole:
        return pathAt(index.row());
    case NameRole:
        return row->name;
    case KindRole:
        return row->kind < m_kinds.size() ? m_kinds.at(row->kind) : QString::number(row->kind);
    case SizeRole:
        return qint64(row->size);
    case ModifiedRole:
        return row->modifiedMs > 0
                   ? QDateTime::fromMSecsSinceEpoch(row->modifiedMs).toString(Qt::ISODate)
                   : QString();
    case DirectoryRole:
        return row->directory;
    default:
        return {};
    }
}

QHash<int, QByteArray> FamListModel::roleNames() const {
    return {
        {NameRole, "name"},
        {KindRole, "kind"},
        {SizeRole, "size"},
        {ModifiedRole, "modified"},
        {PathRole, "path"},
        {DirectoryRole, "directory"},
    };
}

QString FamListModel::pathAt(int index) const {
    const FamViewRow *row = rowAt(index);
    if (row == nullptr) {
        return QString();
    }
    if (!row->path.isEmpty()) {
        return row->path;
    }
    if (m_directory.isEmpty()) {
        return QString();
    }
    /* One join per request instead of one stored string per row: a directory view has exactly one
     * prefix, and copying it into 78k rows was most of v1's per-row memory. */
    return QDir(m_directory).filePath(row->name);
}

int FamListModel::applyCuts(const QVector<quint32> &positions) {
    if (positions.isEmpty() || m_total == 0) {
        return 0;
    }
    /* Positions are already relative to the view as the earlier ones left it, so a run of equal
     * numbers is a block of neighbours and one pair of signals covers all of it. */
    int pairs = 0;
    for (int index = 0; index < positions.size();) {
        const int first = qMin(int(positions.at(index)), int(m_total));
        int run = 1;
        while (index + run < positions.size() && positions.at(index + run) == positions.at(index)) {
            ++run;
        }
        const int last = qMin(first + run - 1, int(m_total) - 1);
        if (first <= last) {
            beginRemoveRows(QModelIndex(), first, last);
            endRemoveRows();
            ++pairs;
        }
        index += run;
    }
    m_total -= quint32(positions.size());
    /* The window is dropped once the burst is over, not between ranges: a repaint that lands inside
     * the loop would otherwise refetch against a row count that is still mid-update. */
    m_first = -1;
    m_rows.clear();
    return pairs;
}
