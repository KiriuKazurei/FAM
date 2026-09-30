#include "tagdialog.h"

#include <QDir>
#include <QFileInfo>
#include <QHBoxLayout>
#include <QJsonArray>
#include <QJsonObject>
#include <QLabel>
#include <QLineEdit>
#include <QListWidget>
#include <QPushButton>
#include <QVBoxLayout>
#include <QVector>

#include "bridge.h"
#include "theme.h"

namespace {

/* One entry per tag *name*, with the sources carrying it named alongside.
 *
 * That grouping is not a display preference - it is what the wire does. `fam_tag_remove` with a null
 * source deletes `tag IN (...)` with no source clause, so removing the name `contract` takes away the
 * manual row *and* the auto row that happens to share it. Showing the two sources as two rows a
 * person could delete separately would promise a control that does not exist, so the dialog presents
 * the only thing that is actually addressable: the name, and everywhere it appears. */
QVector<QPair<QString, QStringList>> groupByName(const QJsonArray &records) {
    QVector<QPair<QString, QStringList>> grouped;
    for (const QJsonValue &value : records) {
        const QJsonObject record = value.toObject();
        const QString tag = record.value(QStringLiteral("tag")).toString();
        const QString source = record.value(QStringLiteral("source")).toString();
        bool merged = false;
        for (auto &entry : grouped) {
            if (entry.first == tag) {
                if (!entry.second.contains(source)) {
                    entry.second.append(source);
                }
                merged = true;
                break;
            }
        }
        if (!merged) {
            grouped.append({tag, {source}});
        }
    }
    return grouped;
}

} /* namespace */

TagDialog::TagDialog(FamBridge *bridge, const QString &path, QWidget *parent)
    : QDialog(parent), m_bridge(bridge), m_path(path) {
    setWindowTitle(QStringLiteral("Tags — %1").arg(QFileInfo(path).fileName()));
    /* The page ground, named the way every other surface in this window names its ground. Without
     * the object name the sheet has no rule to match, Fusion answers with the *desktop's* scheme,
     * and a light-mode window opens a dark dialog whose list is white on one and its heading grey
     * on the other - which is what the first screenshot of this dialog showed, with every wire
     * assertion in the same run passing green. */
    setObjectName(QString::fromLatin1(Theme::pageName()));

    auto *heading = new QLabel(QDir::toNativeSeparators(path), this);
    heading->setToolTip(path);

    m_list = new QListWidget(this);
    m_list->setSelectionMode(QAbstractItemView::SingleSelection);

    m_entry = new QLineEdit(this);
    m_entry->setPlaceholderText(QStringLiteral("a new tag"));
    auto *addButton = new QPushButton(QStringLiteral("Add"), this);
    addButton->setObjectName(QString::fromLatin1(Theme::primaryButtonName()));
    m_removeButton = new QPushButton(QStringLiteral("Remove selected"), this);
    m_renameButton = new QPushButton(QStringLiteral("Rename selected"), this);
    m_renameButton->setToolTip(QStringLiteral(
        "Rename this tag on this file - the whole library changes names through a chip on the "
        "filter strip, not here"));

    m_note = new QLabel(this);
    m_note->setWordWrap(true);

    auto *buttons = new QHBoxLayout;
    buttons->addWidget(m_entry, 1);
    buttons->addWidget(addButton);
    buttons->addWidget(m_renameButton);
    buttons->addWidget(m_removeButton);

    auto *closeButton = new QPushButton(QStringLiteral("Close"), this);
    connect(closeButton, &QPushButton::clicked, this, &QDialog::accept);

    auto *layout = new QVBoxLayout(this);
    layout->addWidget(heading);
    layout->addWidget(m_list, 1);
    layout->addLayout(buttons);
    layout->addWidget(m_note);
    layout->addWidget(closeButton, 0, Qt::AlignRight);

    connect(addButton, &QPushButton::clicked, this, &TagDialog::add);
    connect(m_entry, &QLineEdit::returnPressed, this, &TagDialog::add);
    connect(m_removeButton, &QPushButton::clicked, this, &TagDialog::removeSelected);
    connect(m_renameButton, &QPushButton::clicked, this, &TagDialog::renameSelected);
    /* Anything that moves the pick or rewrites the list disarms both two-step buttons: an armed
     * confirm that outlived the row it was armed for would confirm something else. */
    connect(m_list, &QListWidget::itemSelectionChanged, this, &TagDialog::disarmConfirm);

    reload();
}

QStringList TagDialog::tagNames() const {
    return m_names;
}

QStringList TagDialog::tagDisplay() const {
    return m_display;
}

QString TagDialog::noteText() const {
    return m_note != nullptr ? m_note->text() : QString();
}

void TagDialog::setEntryText(const QString &text) {
    if (m_entry != nullptr) {
        m_entry->setText(text);
    }
}

void TagDialog::reload() {
    disarmConfirm();
    QJsonValue result;
    QString error;
    if (!m_bridge->tagList(m_path, &result, &error)) {
        m_note->setText(error);
        m_names.clear();
        m_display.clear();
        showList();
        return;
    }
    m_names.clear();
    m_display.clear();
    for (const auto &entry : groupByName(result.toArray())) {
        m_names.append(entry.first);
        m_display.append(entry.second.isEmpty()
                             ? entry.first
                             : QStringLiteral("%1  (%2)")
                                   .arg(entry.first, entry.second.join(QStringLiteral(", "))));
    }
    showList();
    if (m_names.isEmpty()) {
        m_note->setText(QStringLiteral("this file has no tags yet"));
    } else {
        m_note->clear();
    }
}

void TagDialog::showList() {
    /* The widget only exists to be looked at; `m_names` is the state the operations work from, so a
     * dialog that was never shown still answers `tagNames` the same way a visible one does. */
    if (m_list != nullptr) {
        m_list->clear();
        for (int row = 0; row < m_display.size(); ++row) {
            m_list->addItem(m_display.at(row));
        }
    }
}

bool TagDialog::addTag(const QString &tag, QString *error) {
    const QString trimmed = tag.trimmed();
    if (trimmed.isEmpty()) {
        *error = QStringLiteral("a tag needs some text");
        return false;
    }
    QJsonValue result;
    if (!m_bridge->tagAdd(m_path, QStringList{trimmed}, QStringLiteral("manual"), 0, &result,
                          error)) {
        /* Anything the store rejects on the text itself - a space at either end, more than 64
         * characters - arrives here as its sentence, which is the same sentence the agent gets for
         * the same rule. */
        return false;
    }
    m_changed = true;
    return true;
}

bool TagDialog::renameTag(const QString &from, const QString &to, QString *error) {
    if (!m_names.contains(from)) {
        *error = QStringLiteral("pick a tag to rename first");
        return false;
    }
    const QString trimmed = to.trimmed();
    if (trimmed.isEmpty()) {
        *error = QStringLiteral("a tag needs some text");
        return false;
    }
    QJsonValue result;
    if (!m_bridge->tagRename(m_path, from, trimmed, &result, error)) {
        /* The store's own refusals - an invalid name, or a target this file already carries -
         * arrive as their sentences, the same ones an agent gets. */
        return false;
    }
    m_changed = true;
    return true;
}

void TagDialog::disarmConfirm() {
    m_removeArmed = false;
    m_pendingRemove.clear();
    if (m_removeButton != nullptr) {
        m_removeButton->setText(QStringLiteral("Remove selected"));
    }
}

bool TagDialog::removeTag(const QString &name, QString *error) {
    if (name.trimmed().isEmpty()) {
        /* The footgun this guard exists for: `fam_tag_remove` with an empty tag list deletes every
         * tag on the file, because the SQL drops its `tag IN (...)` clause when the list is empty.
         * "nothing picked" therefore cannot be forwarded as "nothing to remove". */
        *error = QStringLiteral("pick a tag to remove first");
        return false;
    }
    QJsonValue result;
    if (!m_bridge->tagRemove(m_path, QStringList{name}, &result, error)) {
        return false;
    }
    m_changed = true;
    return true;
}

void TagDialog::add() {
    QString error;
    if (!addTag(m_entry->text(), &error)) {
        m_note->setText(error);
        return;
    }
    m_entry->clear();
    reload();
}

void TagDialog::selectFirstRow() {
    if (m_list != nullptr) {
        m_list->setCurrentRow(0);
    }
}

void TagDialog::removeSelected() {
    const int row = m_list->currentRow();
    if (row < 0 || row >= m_names.size()) {
        m_note->setText(QStringLiteral("pick a tag to remove first"));
        return;
    }
    const QString name = m_names.at(row);
    /* Two presses. The first names exactly what the second will do - removal takes the name from
     * this file across both sources - and disarms itself if the selection moves in between. The
     * wire underneath has no undo, which is why the confirmation is here rather than nowhere. */
    if (!m_removeArmed || m_pendingRemove != name) {
        m_removeArmed = true;
        m_pendingRemove = name;
        m_removeButton->setText(QStringLiteral("Really remove"));
        m_note->setText(QStringLiteral("press again to remove '%1' from this file").arg(name));
        return;
    }
    disarmConfirm();
    QString error;
    if (!removeTag(name, &error)) {
        m_note->setText(error);
        return;
    }
    reload();
}

void TagDialog::renameSelected() {
    const int row = m_list->currentRow();
    if (row < 0 || row >= m_names.size()) {
        m_note->setText(QStringLiteral("pick a tag to rename first"));
        return;
    }
    /* The entry is the new name. Empty means "the person has not typed it yet", so the selected
     * name is put there for editing rather than refused - renaming in place is what the gesture
     * means. */
    const QString from = m_names.at(row);
    const QString typed = m_entry->text();
    if (typed.trimmed().isEmpty()) {
        m_entry->setText(from);
        m_entry->selectAll();
        m_entry->setFocus();
        m_note->setText(QStringLiteral("edit the name above, then press Rename again"));
        return;
    }
    QString error;
    if (!renameTag(from, typed, &error)) {
        m_note->setText(error);
        return;
    }
    m_entry->clear();
    reload();
}
