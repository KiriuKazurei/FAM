#include "famlistmodel.h"

#include <QDateTime>
#include <QDir>

#include "iconcache.h"

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

void FamListModel::setIconCache(IconCache *cache) {
    m_icons = cache;
    if (m_icons != nullptr) {
        connect(m_icons, &IconCache::resolved, this, &FamListModel::iconsResolved);
    }
}

void FamListModel::iconsResolved() {
    if (m_first < 0 || m_rows.isEmpty() || m_total == 0) {
        return;
    }
    /* The materialised window is exactly what a paint can put on screen, so it is exactly what a
     * batch of new icons can change. Rows outside it fetch - and find their icon already cached -
     * when they are painted. */
    const int last = qMin(int(m_total) - 1, m_first + int(m_rows.size()) - 1);
    if (m_first <= last) {
        emit dataChanged(index(m_first), index(last), {Qt::DecorationRole});
    }
}

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
    m_state = FAM_EMPTY;
    m_first = -1;
    m_rows.clear();
    endResetModel();
}

void FamListModel::unbind() {
    beginResetModel();
    m_token = 0;
    m_fetch = nullptr;
    m_total = 0;
    m_state = FAM_EMPTY;
    m_first = -1;
    m_rows.clear();
    endResetModel();
}

void FamListModel::readHead(const FamWindow &window) {
    /* A directory's header says nothing until its rows exist. A search's does while it still runs - it
     * reports the rows found so far and flags them a prefix - so that is the case the READY/STALE test
     * would throw away. Either way "loading, with nothing to show" must not reset a view that is
     * already displaying rows. */
    const bool usable =
        window.state == FAM_READY || window.state == FAM_STALE || window.partial();
    if (!usable) {
        return;
    }
    if (window.totalRows == m_total && window.state == m_state) {
        return;
    }
    beginResetModel();
    m_total = window.totalRows;
    m_state = window.state;
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
    case Qt::DecorationRole: {
        if (m_icons == nullptr) {
            return {};
        }
        /* The key is built from what the row already carries - name, attributes - so a paint never
         * opens a path. Only a tier 2 key, whose icon is not a function of its extension, needs the
         * composed path, and only then is it composed. */
        IconKey key = iconKeyFor(row->name, QString(), row->attrs, row->directory);
        if (key.tier2) {
            key.path = pathAt(index.row());
        }
        return m_icons->iconFor(key);
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
    quint32 removed = 0;
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
            /* Counted here rather than taken from `positions.size()`: a position at or past the end
             * is clamped away above, and subtracting it anyway would underflow an unsigned total
             * into a negative `rowCount()`. The model's total can lag the core's - `fetchAt` adopts
             * a new one without signals - so "past the end" is a real state, not a corrupted call. */
            removed += quint32(last - first + 1);
        }
        index += run;
    }
    m_total -= qMin(removed, m_total);
    /* The window is dropped once the burst is over, not between ranges: a repaint that lands inside
     * the loop would otherwise refetch against a row count that is still mid-update. */
    m_first = -1;
    m_rows.clear();
    return pairs;
}

bool FamListModel::sameRow(const FamViewRow &left, const FamViewRow &right) {
    return left.name == right.name && left.path == right.path && left.size == right.size &&
           left.modifiedMs == right.modifiedMs && left.kind == right.kind &&
           left.directory == right.directory && left.attrs == right.attrs;
}

FamListModel::Reconciled FamListModel::reconcile(const FamWindow &window) {
    Reconciled outcome;
    const int oldTotal = int(m_total);
    const int newTotal = int(window.totalRows);
    /* A window that was never materialised has nothing on screen to preserve, and a view whose rows
     * are gone entirely is better served by the plain path. */
    auto rebuild = [&]() {
        readHead(window);
        outcome.rebuilt = true;
        return outcome;
    };
    if (m_first < 0 || m_rows.isEmpty() || m_fetch == nullptr || newTotal == 0) {
        return rebuild();
    }

    const int count = qMin(m_viewport, newTotal);
    const int first = qBound(0, m_first, qMax(0, newTotal - count));
    /* The window's own start moving is a shift of everything below it rather than a difference
     * inside it: the two halves of the diff would then describe two different tables. */
    if (first != m_first) {
        return rebuild();
    }
    FamWindow fresh;
    if (!m_fetch(quint32(first), quint32(count), &fresh) ||
        int(fresh.totalRows) != newTotal) {
        /* The core moved again between the poll and this fetch. Reconciling against a table that is
         * already history is how a model ends up lying about its own row count. */
        return rebuild();
    }
    const QVector<FamViewRow> before = m_rows;
    const QVector<FamViewRow> &after = fresh.rows;

    /* Everything that differs sits between a common prefix and a common suffix - which is exactly
     * what a re-read of a sorted table does to a window: a new row lands in one place, and a row
     * that only got bigger is not the same row (see `sameRow`). */
    int prefix = 0;
    while (prefix < before.size() && prefix < after.size() &&
           sameRow(before.at(prefix), after.at(prefix))) {
        ++prefix;
    }
    int suffix = 0;
    while (suffix < before.size() - prefix && suffix < after.size() - prefix &&
           sameRow(before.at(before.size() - 1 - suffix), after.at(after.size() - 1 - suffix))) {
        ++suffix;
    }
    const int gone = before.size() - prefix - suffix;
    const int arrived = after.size() - prefix - suffix;
    /* Rows outside the window changed too - that is what moves the total - and `rowCount` answers
     * with the whole view, so the remainder has to be announced somewhere. The end of the model is
     * where no delegate is painting. */
    const int outside = (newTotal - oldTotal) - (arrived - gone);
    const int tailAfterGone = oldTotal - gone;
    if (gone < 0 || arrived < 0 || first + before.size() > oldTotal ||
        (outside < 0 && first + prefix + suffix > tailAfterGone + outside)) {
        /* The tail removal would reach into the window, which means the diff and the arithmetic are
         * describing different rows. Rare enough to hand back to a rebuild rather than to reason
         * about here. */
        return rebuild();
    }

    int pairs = 0;
    if (gone > 0) {
        beginRemoveRows(QModelIndex(), first + prefix, first + prefix + gone - 1);
        m_rows = before.mid(0, prefix) + before.mid(before.size() - suffix);
        m_total = quint32(tailAfterGone);
        endRemoveRows();
        ++pairs;
    }
    if (outside < 0) {
        const int at = tailAfterGone + outside;
        beginRemoveRows(QModelIndex(), at, tailAfterGone - 1);
        m_total = quint32(at);
        endRemoveRows();
        ++pairs;
    }
    if (outside > 0) {
        const int at = int(m_total);
        beginInsertRows(QModelIndex(), at, at + outside - 1);
        m_total = quint32(at + outside);
        endInsertRows();
        ++pairs;
    }
    if (arrived > 0) {
        const int at = first + prefix;
        beginInsertRows(QModelIndex(), at, at + arrived - 1);
        m_rows = QVector<FamViewRow>(m_rows.constBegin(), m_rows.constBegin() + prefix) +
                 after.mid(prefix, arrived) +
                 QVector<FamViewRow>(m_rows.constBegin() + prefix, m_rows.constEnd());
        m_total = quint32(int(m_total) + arrived);
        endInsertRows();
        ++pairs;
    }
    /* Whatever the ranges were, the window now holds what the core just handed over, and the count
     * has to land on the header's number: the pairs above are only a bridge to it. */
    m_rows = after;
    m_total = window.totalRows;
    m_state = window.state;
    outcome.pairs = pairs;
    return outcome;
}
