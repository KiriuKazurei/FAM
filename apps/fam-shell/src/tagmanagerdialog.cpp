#include "tagmanagerdialog.h"

#include <QDir>
#include <QDebug>
#include <QFileInfo>
#include <QDebug>
#include <QHBoxLayout>
#include <QJsonObject>
#include <QLabel>
#include <QLineEdit>
#include <QListWidget>
#include <QPushButton>
#include <QVBoxLayout>
#include <QVector>

#include "bridge.h"
#include "theme.h"

TagManagerDialog::TagManagerDialog(FamBridge *bridge, const QString &currentDir,
                                   QWidget *parent)
    : QDialog(parent), m_bridge(bridge), m_currentDir(currentDir) {
    const QString scope = currentDir.isEmpty()
        ? QStringLiteral("everywhere")
        : QDir::toNativeSeparators(currentDir);
    setWindowTitle(QStringLiteral("Tag manager — %1").arg(scope));
    setObjectName(QString::fromLatin1(Theme::pageName()));

    auto *heading = new QLabel(
        QStringLiteral("every tag in the library — the browsed directory's own tags are listed first"),
        this);
    heading->setWordWrap(true);

    m_list = new QListWidget(this);
    m_list->setSelectionMode(QAbstractItemView::SingleSelection);

    m_entry = new QLineEdit(this);
    m_entry->setPlaceholderText(QStringLiteral("a new name — rename into it, or merge into it"));
    auto *applyButton = new QPushButton(QStringLiteral("Rename / merge"), this);
    applyButton->setObjectName(QString::fromLatin1(Theme::primaryButtonName()));
    m_deleteButton = new QPushButton(QStringLiteral("Delete tag"), this);
    m_deleteButton->setToolTip(
        QStringLiteral("Remove the selected tag from every file - this cannot be undone"));

    m_line = new QLabel(this);
    m_line->setWordWrap(true);

    auto *buttons = new QHBoxLayout;
    buttons->addWidget(m_entry, 1);
    buttons->addWidget(applyButton);
    buttons->addWidget(m_deleteButton);

    auto *closeButton = new QPushButton(QStringLiteral("Close"), this);
    connect(closeButton, &QPushButton::clicked, this, &QDialog::accept);

    auto *layout = new QVBoxLayout(this);
    layout->addWidget(heading);
    layout->addWidget(m_list, 1);
    layout->addLayout(buttons);
    layout->addWidget(m_line);
    layout->addWidget(closeButton, 0, Qt::AlignRight);

    connect(applyButton, &QPushButton::clicked, this, &TagManagerDialog::applyClicked);
    connect(m_deleteButton, &QPushButton::clicked, this, &TagManagerDialog::deleteClicked);
    connect(m_list, &QListWidget::itemSelectionChanged, this, [this] {
        /* A different tag invalidates the armed delete and the entry both: the armed state
         * names one row, and a rename typed for another row must not fire on it. */
        m_deleteArmed = false;
        m_deleteButton->setText(QStringLiteral("Delete tag"));
    });

    reload();
}

QStringList TagManagerDialog::tagNames() const {
    return m_names;
}

QString TagManagerDialog::selectedTag() const {
    return selectedName();
}

QString TagManagerDialog::selectedName() const {
    const int row = m_list != nullptr ? m_list->currentRow() : -1;
    return row >= 0 && row < m_rawNames.size() ? m_rawNames.at(row) : QString();
}

QString TagManagerDialog::entryText() const {
    return m_entry != nullptr ? m_entry->text() : QString();
}

void TagManagerDialog::setEntryText(const QString &text) {
    if (m_entry != nullptr) {
        m_entry->setText(text);
    }
}

bool TagManagerDialog::selectFirstRow() {
    if (m_list == nullptr || m_list->count() == 0) {
        return false;
    }
    m_list->setCurrentRow(0);
    return true;
}

QString TagManagerDialog::noteLine() const {
    return m_line != nullptr ? m_line->text() : QString();
}

void TagManagerDialog::showList() {
    if (m_list != nullptr) {
        m_list->clear();
        for (const QString &name : m_names) {
            m_list->addItem(name);
        }
    }
}

void TagManagerDialog::reload() {
    m_names.clear();
    m_rawNames.clear();
    QVector<TagFacet> facets;
    QString error;
    if (!m_bridge->tagVocabulary(&facets, &error)) {
        m_line->setText(error);
        showList();
        return;
    }
    /* The browsed directory's own labels first, then the library-wide rest - the section the
     * person is likely acting on leads. */
    QVector<TagFacet> scoped;
    if (!m_currentDir.isEmpty()) {
        m_bridge->tagVocabularyWithin(m_currentDir, &scoped, &error);
    }
    for (const TagFacet &facet : std::as_const(scoped)) {
        m_rawNames.append(facet.tag);
        m_names.append(QStringLiteral("%1  ·  %2  —  in this directory")
                           .arg(facet.tag)
                           .arg(facet.files));
    }
    for (const TagFacet &facet : std::as_const(facets)) {
        const bool inScope = std::any_of(scoped.cbegin(), scoped.cend(),
                                         [&facet](const TagFacet &s) { return s.tag == facet.tag; });
        if (!inScope) {
            m_rawNames.append(facet.tag);
            m_names.append(QStringLiteral("%1  ·  %2").arg(facet.tag).arg(facet.files));
        }
    }
    qDebug("tag mgr reload: %d library, %d scoped, %d names", facets.size(), scoped.size(), m_names.size());
    disarmDelete();
    showList();
    if (m_names.isEmpty()) {
        m_line->setText(QStringLiteral("the library holds no tags yet"));
    }
}

bool TagManagerDialog::applyMerge(const QString &target, QString *error) {
    const QString from = selectedName();
    if (from.isEmpty()) {
        *error = QStringLiteral("pick a tag first");
        return false;
    }
    const QString trimmed = target.trimmed();
    if (trimmed.isEmpty()) {
        *error = QStringLiteral("a name needs some text");
        return false;
    }
    if (trimmed == from) {
        *error = QStringLiteral("that is the name it already has");
        return false;
    }
    QJsonValue result;
    if (!m_bridge->tagMerge(from, trimmed, &result, error)) {
        return false;
    }
    const QJsonObject outcome = result.toObject();
    m_line->setText(QStringLiteral("%1 -> %2: %3 row(s) moved, %4 collapsed")
                        .arg(from, trimmed)
                        .arg(outcome.value(QStringLiteral("moved")).toInt())
                        .arg(outcome.value(QStringLiteral("collapsed")).toInt()));
    m_changed = true;
    reload();
    return true;
}

void TagManagerDialog::disarmDelete() {
    m_deleteArmed = false;
    if (m_deleteButton != nullptr) {
        m_deleteButton->setText(QStringLiteral("Delete tag"));
    }
}

bool TagManagerDialog::deleteTag(QString *error) {
    const QString name = selectedName();
    if (name.isEmpty()) {
        *error = QStringLiteral("pick a tag first");
        return false;
    }
    /* Two presses: the first names the scope of the removal - every file, both sources - and
     * nothing happens until the second. The reload disarms; the note line carries the count. */
    if (!m_deleteArmed) {
        m_deleteArmed = true;
        m_deleteButton->setText(QStringLiteral("Really delete"));
        m_line->setText(QStringLiteral("press again to remove '%1' from every file").arg(name));
        return false;
    }
    disarmDelete();
    QJsonValue result;
    if (!m_bridge->tagDelete(name, &result, error)) {
        return false;
    }
    const int removed = result.toObject().value(QStringLiteral("removed")).toInt();
    m_line->setText(QStringLiteral("removed %1 row(s) named '%2'").arg(removed).arg(name));
    m_changed = true;
    reload();
    return true;
}

void TagManagerDialog::applyClicked() {
    QString error;
    if (!applyMerge(entryText(), &error)) {
        m_line->setText(error);
        return;
    }
    m_entry->clear();
}

void TagManagerDialog::deleteClicked() {
    QString error;
    deleteTag(&error);
}
