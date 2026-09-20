#include "famlistmodel.h"

#include <QDateTime>

QString formatSize(qint64 bytes) {
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
    return parent.isValid() ? 0 : m_rows.size();
}

QVariant FamListModel::data(const QModelIndex &index, int role) const {
    if (!index.isValid() || index.row() < 0 || index.row() >= m_rows.size()) {
        return {};
    }
    const FamRow &row = m_rows.at(index.row());
    switch (role) {
    case Qt::DisplayRole: {
        QString display = QStringLiteral("%1  [%2]  %3").arg(row.name, row.kind, formatSize(row.size));
        if (!row.tags.isEmpty()) {
            display += QStringLiteral("  # %1").arg(row.tags);
        }
        return display;
    }
    case Qt::ToolTipRole:
        return row.path;
    case KeyRole:
        return row.key;
    case PathRole:
        return row.path;
    case NameRole:
        return row.name;
    case KindRole:
        return row.kind;
    case SizeRole:
        return row.size;
    case ModifiedRole:
        return row.modifiedMs > 0
                   ? QDateTime::fromMSecsSinceEpoch(row.modifiedMs).toString(Qt::ISODate)
                   : QString();
    case TagsRole:
        return row.tags;
    default:
        return {};
    }
}

QHash<int, QByteArray> FamListModel::roleNames() const {
    return {
        {KeyRole, "key"},
        {PathRole, "path"},
        {NameRole, "name"},
        {KindRole, "kind"},
        {SizeRole, "size"},
        {ModifiedRole, "modified"},
        {TagsRole, "tags"},
    };
}

FamRow FamListModel::rowFromJson(const QJsonObject &object) {
    FamRow row;
    row.key = object.value("key").toString();
    row.path = object.value("path").toString();
    row.name = object.value("name").toString();
    row.kind = object.value("kind").toString();
    row.size = static_cast<qint64>(object.value("size").toDouble());
    row.modifiedMs = static_cast<qint64>(object.value("modified_ms").toDouble());
    const QJsonArray tags = object.value("tags").toArray();
    QStringList parts;
    for (const QJsonValue &tag : tags) {
        parts.append(tag.toString());
    }
    row.tags = parts.join(QStringLiteral(", "));
    return row;
}

void FamListModel::load(const QJsonArray &rows) {
    beginResetModel();
    m_rows.clear();
    m_rows.reserve(static_cast<int>(rows.size()));
    for (const QJsonValue &value : rows) {
        m_rows.append(rowFromJson(value.toObject()));
    }
    endResetModel();
}

void FamListModel::clearRows() {
    beginResetModel();
    m_rows.clear();
    endResetModel();
}

void FamListModel::applyDeltas(const QJsonArray &deltas) {
    for (const QJsonValue &value : deltas) {
        const QJsonObject object = value.toObject();
        const QString type = object.value("type").toString();
        const int index = object.value("index").toInt(-1);

        if (type == QLatin1String("insert")) {
            if (index < 0 || index > m_rows.size()) {
                continue;
            }
            beginInsertRows(QModelIndex(), index, index);
            m_rows.insert(index, rowFromJson(object.value("row").toObject()));
            endInsertRows();
        } else if (type == QLatin1String("remove")) {
            if (index < 0 || index >= m_rows.size()) {
                continue;
            }
            beginRemoveRows(QModelIndex(), index, index);
            m_rows.remove(index);
            endRemoveRows();
        } else if (type == QLatin1String("change")) {
            if (index < 0 || index >= m_rows.size()) {
                continue;
            }
            m_rows[index] = rowFromJson(object.value("row").toObject());
            emit dataChanged(this->index(index), this->index(index));
        } else if (type == QLatin1String("reset")) {
            clearRows();
        }
    }
}

QString FamListModel::pathAt(int index) const {
    if (index < 0 || index >= m_rows.size()) {
        return QString();
    }
    return m_rows.at(index).path;
}
