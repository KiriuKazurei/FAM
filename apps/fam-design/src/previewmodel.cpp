#include "previewmodel.h"

#include <QDate>
#include <QDateTime>
#include <QDir>
#include <QFileInfo>

#include <algorithm>

namespace {

/* The buckets are `fam_core::Kind`'s, in its declaration order, so a tag here names the same group
 * the product would name. Only the extension table below is the preview's own - the core decides it
 * in `fam-svc-enumerate`, and duplicating that decision would be a design sample quietly disagreeing
 * with the thing it is meant to preview. */
constexpr int kDirectory = 0;
constexpr int kImage = 1;
constexpr int kVideo = 2;
constexpr int kAudio = 3;
constexpr int kDocument = 4;
constexpr int kArchive = 5;
constexpr int kCode = 6;
constexpr int kExecutable = 7;
constexpr int kOther = 8;

int bucketOf(const QString &name) {
    static const QHash<QString, int> table {
        {"png", kImage},      {"jpg", kImage},     {"jpeg", kImage}, {"gif", kImage},
        {"webp", kImage},     {"bmp", kImage},     {"svg", kImage},  {"heic", kImage},
        {"mp4", kVideo},      {"mkv", kVideo},     {"mov", kVideo},  {"avi", kVideo},
        {"mp3", kAudio},      {"flac", kAudio},    {"wav", kAudio},  {"ogg", kAudio},
        {"pdf", kDocument},   {"doc", kDocument},  {"docx", kDocument}, {"xls", kDocument},
        {"xlsx", kDocument},  {"ppt", kDocument},  {"pptx", kDocument}, {"md", kDocument},
        {"txt", kDocument},   {"csv", kDocument},  {"epub", kDocument},
        {"zip", kArchive},    {"rar", kArchive},   {"7z", kArchive}, {"tar", kArchive},
        {"gz", kArchive},     {"bz2", kArchive},   {"xz", kArchive},  {"tgz", kArchive},
        {"rs", kCode},        {"py", kCode},       {"js", kCode},     {"ts", kCode},
        {"tsx", kCode},       {"go", kCode},       {"c", kCode},      {"cc", kCode},
        {"cpp", kCode},       {"h", kCode},        {"hpp", kCode},    {"java", kCode},
        {"toml", kCode},      {"json", kCode},     {"yaml", kCode},   {"yml", kCode},
        {"xml", kCode},       {"sql", kCode},      {"sh", kCode},     {"bat", kCode},
        {"exe", kExecutable}, {"dll", kExecutable}, {"msi", kExecutable}, {"appimage", kExecutable},
    };
    const QString suffix = QFileInfo(name).suffix().toLower();
    return table.value(suffix, kOther);
}

} /* namespace */

namespace preview {

QString kindName(int kind) {
    switch (kind) {
    case kDirectory:
        return QStringLiteral("directory");
    case kImage:
        return QStringLiteral("image");
    case kVideo:
        return QStringLiteral("video");
    case kAudio:
        return QStringLiteral("audio");
    case kDocument:
        return QStringLiteral("document");
    case kArchive:
        return QStringLiteral("archive");
    case kCode:
        return QStringLiteral("code");
    case kExecutable:
        return QStringLiteral("executable");
    default:
        return QStringLiteral("other");
    }
}

int kindFor(bool directory, const QString &name) {
    return directory ? kDirectory : bucketOf(name);
}

QString formatSize(quint64 bytes) {
    static const char *const units[] = {"B", "KB", "MB", "GB", "TB"};
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

QString formatWhen(qint64 epochMs) {
    if (epochMs <= 0) {
        return QString();
    }
    const QDateTime when = QDateTime::fromMSecsSinceEpoch(epochMs);
    const QDate day = when.date();
    const QDate today = QDate::currentDate();
    /* Today gets a clock, this year gets a month and a day, and anything older gets the year: a list
     * of files sorts meaningfully by "when did I last touch this", and the year is what answers that
     * for the files nobody has touched lately. */
    if (day == today) {
        return when.toString(QStringLiteral("HH:mm"));
    }
    if (day.year() == today.year()) {
        return day.toString(QStringLiteral("MM-dd"));
    }
    return day.toString(QStringLiteral("yyyy-MM-dd"));
}

} /* namespace preview */

PreviewModel::PreviewModel(QObject *parent) : QAbstractListModel(parent) {}

int PreviewModel::load(const QString &directory, const QString &needle, int kind,
                       const QString &sortKey) {
    beginResetModel();
    m_directory = directory;
    m_rows.clear();
    const QDir source(directory);
    m_readable = source.exists();
    const QFileInfoList entries =
        source.entryInfoList(QDir::AllEntries | QDir::NoDotAndDotDot | QDir::Hidden, QDir::Name);
    for (const QFileInfo &entry : entries) {
        if (!needle.isEmpty() && !entry.fileName().contains(needle, Qt::CaseInsensitive)) {
            continue;
        }
        Entry row;
        row.name = entry.fileName();
        row.path = entry.absoluteFilePath();
        row.directory = entry.isDir();
        row.size = entry.isDir() ? 0 : entry.size();
        row.modifiedMs = entry.lastModified().toMSecsSinceEpoch();
        row.kind = preview::kindFor(row.directory, row.name);
        if (kind >= 0 && row.kind != kind) {
            continue;
        }
        m_rows.append(row);
    }
    /* Folders first: in a Miller column the directories are how you move, so they belong at the top
     * of the viewport rather than scattered among the files by name. */
    std::stable_sort(m_rows.begin(), m_rows.end(),
                     [&sortKey](const Entry &left, const Entry &right) {
        if (left.directory != right.directory) {
            return left.directory;
        }
        if (sortKey == QLatin1String("size")) {
            return left.size < right.size;
        }
        if (sortKey == QLatin1String("time")) {
            return left.modifiedMs < right.modifiedMs;
        }
        return left.name.localeAwareCompare(right.name) < 0;
    });
    endResetModel();
    return m_rows.size();
}

int PreviewModel::rowCount(const QModelIndex &parent) const {
    return parent.isValid() ? 0 : m_rows.size();
}

QVariant PreviewModel::data(const QModelIndex &index, int role) const {
    if (!index.isValid() || index.row() >= m_rows.size()) {
        return {};
    }
    const Entry &row = m_rows.at(index.row());
    switch (role) {
    case Qt::DisplayRole:
        return row.name;
    case NameRole:
        return row.name;
    case KindRole:
        return preview::kindName(row.kind);
    case SizeRole:
        return static_cast<qint64>(row.size);
    case ModifiedRole:
        return preview::formatWhen(row.modifiedMs);
    case PathRole:
    case Qt::ToolTipRole:
        return row.path;
    case DirectoryRole:
        return row.directory;
    default:
        return {};
    }
}

QHash<int, QByteArray> PreviewModel::roleNames() const {
    return {
        {NameRole, "name"},
        {KindRole, "kind"},
        {SizeRole, "size"},
        {ModifiedRole, "modified"},
        {PathRole, "path"},
        {DirectoryRole, "directory"},
    };
}
